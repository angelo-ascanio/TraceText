# TraceText — Structural Document Search

TraceText is a high-performance, structurally-aware desktop utility designed to cross-reference and audit text queries against dense PDF and DOCX files. Built entirely in Rust, it combines fuzzy matching capabilities with structural tracking, allowing compliance officers, technical analysts, and developers to trace requirements directly back to their source coordinates.

## 🚀 Features

* **Multi-Format Structural Extraction:** Parses text while preserving contextual anchors:
    * **PDFs:** Tracks exact `Page Number` and physical `Block Index` using a streaming architecture.
    * **DOCX:** Captures `Global Paragraph Index` and dynamically infers the active `Heading Context`.
* **High-Performance Search Pipeline:** * Uses **Aho-Corasick** for rapid sub-linear initial filtering.
    * Implements **Nucleo-Matcher** (the engine behind advanced fuzzy finders) for smart, case-insensitive, normalized fuzzy distance scoring.
    * Leverages **Rayon** for data-parallel candidate evaluation across all available CPU cores.
* **Robust Text Normalization:** Strips combining marks, applies Unicode normalization (NFD/NFKC), and flattens text casings to eliminate false negatives driven by formatting differences.
* **Low-Friction Desktop GUI:** Built using an immediate-mode `egui` framework with an expandable virtualized results table.
* **Seamless Data Export:** Copy multi-line results directly to your clipboard as clean TSV data or export structured analytical files directly to Microsoft Excel (`.xlsx`).

---

## 🛠️ Architecture Overview

TraceText executes its processing pipeline across four streamlined steps:

1.  **Ingestion:** The document is loaded dynamically through format-specific stream abstractions (`unpdf` / `undoc`) without inflating total memory footprint overhead unnecessarily.
2.  **Structural Mapping:** Sentences are mapped directly into a `StructuralLocation` enum variant, preserving exactly where the string lives in the legal or technical text framework.
3.  **Parallel Fuzzy Scoring:** Candidate text items are scored concurrently against a batch of target queries. Match spans are capped dynamically to eliminate sprawling, low-relevance blocks.
4.  **Tabular Reporting:** Scores are compiled into an intuitive tabular UI showing explicit Match Flags (Yes/No), similarity scores, and contextual text snippets.

---

## 📦 Prerequisites & Dependencies

To compile TraceText from source, you will need the stable Rust toolchain (**Edition 2024**).

The core engine relies on the following key dependencies:
* `eframe` & `egui_extras` - For the native multi-platform desktop rendering engine.
* `nucleo-matcher` - High-fidelity sub-string alignment and scoring.
* `rayon` - Core-bound thread pool processing.
* `rust_xlsxwriter` - Native, zero-dependency Excel workbook creation.

---

## 🚀 Getting Started

### 1. Clone the repository
```bash
git clone [https://github.com/angelo-ascanio/TraceText.git](https://github.com/angelo-ascanio/TraceText.git)
cd TraceText

```

### 2. Build and Run

For production use, always execute compilation with the `--release` flag. This ensures that the heavy text normalization blocks, fuzzy matching matrices, and parallel iterator structures are optimized for maximum speed:

```bash
cargo run --release

```

---

## 🎛️ Windows Optimization (Hiding the Console)

By default, launching a GUI app on Windows triggers a companion command prompt window. TraceText bypasses this in production by including the following conditional attribute at the crown of `main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

```

This guarantees that dev builds (`cargo run`) retain the terminal for stdout/stderr logs, while release binaries (`cargo run --release`) launch directly into a clean, windowed UX.

---

## 📖 Usage Guide

1. **Select a Target File:** Click **"Select PDF/Word Document"** to pull up a native file picker. Select any `.pdf` or `.docx` file.
2. **Define Search Queries:** Type or paste your tracking benchmarks, clause items, or regex-like string targets into the multiline editor (**one query per line**).
3. **Set the Similarity Threshold:** Adjust the slider to tune match tolerance.
* **`100%`** demands absolute character alignments.
* **`85% - 95%`** gracefully absorbs typo variations, character omissions, or minor spacing differences typical of PDF layout reflows.


4. **Execute:** Click **"Run Search"** to run the parallel search array.
5. **Extract Results:** * Use **"Copy Table"** to instantly move the results grid onto your clipboard formatted as Tab-Separated Values (TSV)—perfect for dropping into target spreadsheets.
* Click **"Export to Excel"** to generate a beautifully structured, native spreadsheet report automatically.

---

## 📜 License

This project is licensed under the MIT License. See the [LICENSE](https://www.google.com/search?q=LICENSE) file for details.

---