# CrateFind Architecture

## Data Flow

```
                                    ┌─────────────────┐
                                    │  crates.io      │
                                    │  db-dump        │
                                    └────────┬────────┘
                                             │
                                             ▼
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│ Cargo.lock  │───▶│  parse.rs   │───▶│  profile.rs │──▶ RawMetrics ──▶ [f32; 8]
│ ~/.cargo/   │    │  (syn AST)  │    │  (static    │     │
│ registry/   │    │             │    │   analysis) │     │
└─────────────┘    └─────────────┘    └─────────────┘     │
                                                          ▼
                                                   ┌─────────────┐
                                                   │ octo_index  │
                                                   │ OctoIndex   │──▶ octo-index.bin (zstd)
                                                   │ {name→[8]}  │
                                                   └─────────────┘
```

## Embedding Pipeline

```
Query String ──▶ all-MiniLM-L6-v2 ──▶ [f32; 384] ──▶ ContrastiveMapper ──▶ [f32; 8]
                 (embed.rs)            │              (contrastive.rs)      │
                 22MB ONNX             │              12KB matrix           │
                                       │                                    ▼
                                       │                            OctoIndex.search()
                                       ▼
                                 db.rs cosine_similarity()
                                 (symbol-level search)
```

## Key Files

| File | Purpose | Key Types |
|------|---------|-----------|
| `embed.rs` | all-MiniLM wrapper | `Embedder`, `Embedding = Vec<f32>` |
| `contrastive.rs` | 384→8 projection | `ContrastiveMapper { weights: [[f32;8];384], bias: [f32;8] }` |
| `octo_index.rs` | Crate characteristic index | `OctoIndex`, `OctonionProfile { coeffs: [f32;8] }` |
| `profile.rs` | Static analysis → metrics | `CrateProfile`, `RawMetrics` |
| `db.rs` | SQLite symbol index | `Database`, cosine search |
| `parse.rs` | Rust AST → symbols | `CrateApi`, `SymbolDoc` |

## 8D Octonion Dimensions

```
e0: utility      downloads/age (log scale)
e1: concurrency  Send/Sync impl count
e2: safety       unsafe blocks / LoC (lower = safer)
e3: async        async fn ratio
e4: memory       heap-allocating types
e5: friction     dependency count
e6: environment  no_std flag (binary)
e7: entropy      version volatility
```

## CLI Commands

```
# Symbol-level (384D cosine)
cratefind index              # parse deps → embed symbols → SQLite
cratefind search "query"     # 384D cosine similarity

# Crate-level (8D octonion)
cratefind profile <crate>    # show 8D coefficients
cratefind octo-index --async --safe -f index.bin
cratefind octo-lookup tokio -f index.bin

# Contrastive (384D → 8D learned)
cratefind train-mapper -i octo-index.bin -o mapper.bin
cratefind semantic-search "async http client" -m mapper.bin -i octo-index.bin
```

## File Formats

**contrastive-mapper.bin** (12,324 bytes)
```
[0..4]     "CMAP" magic
[4..12292] weights: 384 * 8 * 4 bytes (row-major f32 LE)
[12292..]  bias: 8 * 4 bytes
```

**octo-index.bin** (zstd compressed JSON)
```
[0..4]  "OCTO" magic
[4..]   zstd(json{ version, generated_at, count, profiles: {name → OctonionProfile} })
```

## Training

```rust
// Input:  384D embedding of "tokio - Rust crate async asynchronous thread-safe..."
// Target: [0.8, 0.9, 0.1, 0.95, 0.3, 0.4, 0.0, 0.2] from static analysis
// Loss:   MSE after sigmoid
// Update: vanilla SGD, lr=0.5, epochs=1000
```

## Bundled Assets

```
models/all-MiniLM-L6-v2/
├── model.onnx          # x86-64
├── model-arm64.onnx    # ARM64
├── tokenizer.json
├── config.json
├── special_tokens_map.json
└── tokenizer_config.json
```

Feature flags (stubbed):
- `bundled-index` → include_bytes!("../octo-index.bin")
- `bundled-mapper` → include_bytes!("../contrastive-mapper.bin")
