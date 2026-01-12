Dream Tool: symbol.search(query)
Input
Natural language, fuzzy, context-aware:


"ChildBuilder in bevy"
"the thing you pass to with_children closure"
"bevy spawn children parent builder"
"EntityCommands child spawner"
Output

Results for "ChildBuilder in bevy" (10 results, 23ms)

1. bevy_ecs::hierarchy::ChildSpawnerCommands (type alias)     [0.94]
   → Alias for RelatedSpawnerCommands<'w, ChildOf>
   → Used in: EntityCommands::with_children(|parent: &mut ChildSpawnerCommands|)
   → Why: "ChildBuilder" was renamed to "ChildSpawnerCommands" in 0.15
   
2. bevy_ecs::hierarchy::ChildSpawner (type alias)             [0.89]
   → Alias for RelatedSpawner<'w, ChildOf>  
   → Used in: World-level child spawning (not Commands)
   → Why: Direct World access variant, you probably want #1 for systems

3. bevy_ecs::hierarchy::RelatedSpawnerCommands (struct)       [0.72]
   → The underlying generic type
   → Why: Generic parent of ChildSpawnerCommands

4. bevy_ecs::spawn::SpawnRelatedBundle (trait)                [0.58]
   → Why: Related to child spawning infrastructure

─────────────────────────────────────────────────────────────
DEPRECATION NOTICE: 
  "ChildBuilder" existed in bevy 0.14, renamed in 0.15.
  Migration: ChildBuilder → ChildSpawnerCommands
─────────────────────────────────────────────────────────────
Key Features I'd Want
Rename/deprecation awareness - Know that ChildBuilder became ChildSpawnerCommands
Usage context - Show where it's used (with_children closure signature)
World vs Commands variants - Flag which is for systems vs direct World access
"Why matched" explanation - Especially for fuzzy/semantic matches
Cross-version knowledge - "This was X in 0.14, now Y in 0.17"
Trait method search - Find methods on traits, not just the trait itself
Signature search - "function that takes &mut Commands and returns Entity"
Bonus: Contextual Mode
If it knew I was in a system using Commands:


symbol.search("spawn children", context="Commands-based system")
→ Prioritizes ChildSpawnerCommands over ChildSpawner
→ Shows example: commands.spawn(...).with_children(|parent| { ... })
The core insight: I'm usually not looking for a name, I'm looking for a capability. "How do I spawn child entities from a system?" The tool should understand intent, not just match strings.

----

1. "Why did this match?" Explanations (Partially in dream)
The dream mentions this, but I'd go further:


"ChildBuilder" → ChildSpawnerCommands  [renamed in 0.15]
"serialize"   → serde::Serialize       [exact]
"json parse"  → serde_json::from_str   [capability: parses JSON strings]
Not just how it matched (fuzzy, exact) but why this is what you want.

2. Trait Method Resolution

find("push", context="Vec<T>")
→ Vec::push(&mut self, T)
→ But ALSO: Extend::extend, IntoIterator impls...
Currently fastdeps indexes items but doesn't deeply resolve trait method implementations. "What can I call on this type?" is the real question.

3. Inverse Lookups: "What uses this?"

find.usages("Component", crate="bevy_ecs")
→ 847 structs derive Component
→ Top users: Transform, Visibility, GlobalTransform...
Not just "find X" but "find everything that depends on X."

4. Signature Pattern Matching (In dream, worth emphasizing)

find("fn(&mut World) -> Entity")
find("impl<T: Component>")
The ability to search by shape of types, not just names. "I need a function that takes Commands and returns something I can chain."

5. Cross-Version Changelog Synthesis

diff("bevy", "0.14", "0.15")
→ ChildBuilder → ChildSpawnerCommands (renamed)
→ Timer::tick now takes Duration (was f32)
→ NEW: Required components system
Fastdeps already has moved_from, deprecated, since, until in the schema - but no tooling to query across versions.

6. "Show me how this is used" - Example Extraction

example("ChildSpawnerCommands")
→ Found 3 patterns in bevy examples:
  1. commands.spawn(...).with_children(|parent| { parent.spawn(...); })
  2. parent.spawn_batch([...])
  ...
Not API docs, but actual usage patterns from the crate's own examples/tests.

7. Ecosystem-Aware Aliases

find("ECS entity", ecosystem="bevy")  → Entity, EntityCommands, EntityRef
find("ECS entity", ecosystem="hecs")  → Entity, EntityBuilder
find("ORM model", ecosystem="diesel") → table!, Queryable, Insertable
Different ecosystems have different names for the same concepts.

8. Deprecation Chains

trace_deprecation("ChildBuilder")
→ bevy 0.13: ChildBuilder
→ bevy 0.14: ChildBuilder (unchanged)
→ bevy 0.15: RENAMED to ChildSpawnerCommands
→ bevy 0.16: ChildSpawnerCommands (stable)
9. "Complete my thought" - Context Completion

find.complete("commands.spawn(Camera3d).insert")
→ .insert::<Bundle>()
→ .insert_if() 
→ .insert_by_id()
Given partial code, what could come next?

10. Relationship Graphs

graph("Camera3d")
→ REQUIRES: Transform, GlobalTransform, Camera
→ OPTIONAL: Projection, CameraRenderGraph
→ RELATED: Camera2d (sibling), Camera (parent trait)
The schema already has Relation and RelationKind - surface this!

The Meta-Wish: Intent Language
Your dream's deepest insight is this:

"I'm usually not looking for a name, I'm looking for a capability."

The real dream is a tool that speaks intent language:

"spawn children" → capability
"from a system" → context
"using Commands" → constraint
This is fundamentally different from grep. It's closer to asking a colleague who knows the codebase.

The current fastdeps has solid bones: FTS5 search, fuzzy matching, crate relationships, direct vs transitive deps, and the schema already includes lifecycle fields (since, until, moved_from, deprecated). The infrastructure for the dream exists - it's about building the intelligence layer on top.