#!/usr/bin/env python3
"""Regenerate src/t2s.rs from OpenCC's TSCharacters.txt (Apache-2.0).

    curl -sSL -o TSCharacters.txt \
      https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary/TSCharacters.txt
    python3 tools/generate-t2s.py        # writes src/t2s.rs
"""
import sys

SRC = "TSCharacters.txt"
OUT = "src/t2s.rs"

HEADER = '''//! Traditional → Simplified folding, for matching what a music database returns
//! against what the device stores.
//!
//! **Generated file — do not edit by hand.** See `tools/generate-t2s.py`.
//! Provenance: OpenCC `TSCharacters.txt` (Apache-2.0), single-character entries only.

/// Fold a Traditional Chinese character to its Simplified form, or return it unchanged.
pub fn fold(ch: char) -> char {
    match ch {
%s
        other => other,
    }
}
'''


def main() -> int:
    pairs = []
    for line in open(SRC, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) == 2 and len(parts[0]) == 1 and len(parts[1]) == 1 and parts[0] != parts[1]:
            pairs.append((parts[0], parts[1]))
    pairs.sort()
    rows = []
    for i in range(0, len(pairs), 8):
        rows.append("    " + " ".join(f"'{t}' => '{s}'," for t, s in pairs[i:i + 8]))
    with open(OUT, "w", encoding="utf-8") as handle:
        handle.write(HEADER % "\n".join(rows))
    print(f"{OUT}: {len(pairs)} pairs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
