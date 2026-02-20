#!/usr/bin/env python3
"""Reorganize IMPLEMENTATION_DECISIONS.md — group related questions, renumber, remove duplicates."""

import re
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent / "IMPLEMENTATION_DECISIONS.md"
DST = Path(__file__).resolve().parent.parent / "IMPLEMENTATION_DECISIONS.md"

# ─── Step 1: Parse raw question blocks ───

def parse_raw(text: str) -> dict[str, dict]:
    """Return {old_id: {"title": str, "body": str}} preserving raw markdown."""
    blocks: dict[str, dict] = {}
    cur_id = None
    lines: list[str] = []

    def flush():
        nonlocal cur_id, lines
        if cur_id is None:
            return
        # strip trailing blank / divider lines
        while lines and lines[-1].strip() in ("", "---"):
            lines.pop()
        blocks[cur_id]["body"] = "\n".join(lines)
        cur_id = None
        lines = []

    for raw in text.split("\n"):
        m = re.match(r"^### (\d+\.\d+) (.+)", raw)
        if m:
            flush()
            cur_id = m.group(1)
            blocks[cur_id] = {"title": m.group(2).strip(), "body": ""}
            lines = []
            continue
        if re.match(r"^## \d+\.", raw):
            flush()
            continue
        if cur_id is not None:
            lines.append(raw)

    flush()
    return blocks

# ─── Step 2: New structure ───
# (section_title, [old_question_ids])

STRUCTURE = [
    # ── PART A: SCOPE ──
    ("MVP Scope & Implementation Order", [
        "1.1", "1.2", "1.3",
    ]),
    ("Performance & Hardware Targets", [
        "36.1", "36.2", "36.3", "36.4", "36.5",
    ]),
    ("Licensing & Legal", [
        "73.1", "73.2",
    ]),

    # ── PART B: CRYPTOGRAPHY ──
    ("Cryptography, Keys & Addresses", [
        "2.1", "56.1", "56.2", "56.3",
        "2.2", "56.4", "2.3",
        "56.5", "56.6",
    ]),

    # ── PART C: BRN ──
    ("BRN Engine", [
        "3.1", "3.2", "3.3", "3.4",
        "18.1", "18.2", "18.3", "18.4", "18.5",
        "48.1", "48.2",
        "67.1",
    ]),

    # ── PART D: TRST ──
    ("TRST Lifecycle & Merger Graph", [
        "4.1", "4.2", "4.3", "4.4", "4.5",
        "17.1", "17.2", "17.3", "17.4",
        "17.5", "17.6", "17.7", "17.8",
        "17.9", "17.10", "17.11", "17.12",
        "67.2",
    ]),
    ("TRST Revocation", [
        "47.1", "47.2", "47.3", "47.4",
    ]),

    # ── PART E: TRANSACTIONS & BLOCKS ──
    ("Transaction Architecture", [
        "5.1", "5.2", "5.3", "5.4",
        "32.1", "32.2", "32.3",
    ]),
    ("Block Format & Versioning", [
        "6.1",
        "30.1", "30.2", "30.3",
        "57.1", "57.2", "57.3", "57.4",
        "67.3", "67.4",
    ]),

    # ── PART F: DAG LEDGER ──
    ("DAG Ledger", [
        "6.2", "6.3",
        "23.1", "23.2", "23.3", "23.4", "23.5",
    ]),
    ("Snapshots & Fast Sync", [
        "46.1", "46.2", "46.3",
    ]),

    # ── PART G: CONSENSUS ──
    ("ORV Consensus & Elections", [
        "7.1", "7.2", "7.3",
        "35.1", "35.2", "35.3", "35.4", "35.5", "35.6",
        "60.1", "60.2", "60.3", "60.4",
        "69.1", "69.2", "69.3", "69.4",
    ]),
    ("Representatives", [
        "62.1", "62.2", "62.3", "62.4",
    ]),

    # ── PART H: ANTI-SPAM ──
    ("Proof of Work & Anti-Spam", [
        "10.1", "10.2",
        "24.1", "24.2", "24.3",
        "61.1", "61.2", "61.3",
        "70.1", "70.2", "70.3",
    ]),

    # ── PART I: VERIFICATION ──
    ("Verification System", [
        "8.1", "8.2", "8.3",
        "19.1", "19.2", "19.3", "19.4", "19.5",
        "19.6", "19.7", "19.8", "19.9", "19.10",
        "68.1", "68.2", "68.3", "68.4", "68.5",
    ]),
    ("Endorsements", [
        "45.1", "45.2", "45.3",
        "27.1", "27.2", "27.3",
    ]),
    ("Challenges", [
        "44.1", "44.2", "44.3", "44.4", "44.5",
        "27.4", "27.5", "27.6",
    ]),
    ("Verifier Reputation", [
        "43.1", "43.2", "43.3",
    ]),
    ("VRF & Randomness", [
        "9.1", "9.2",
        "71.1", "71.2",
    ]),

    # ── PART J: GOVERNANCE ──
    ("Governance", [
        "11.1", "11.2",
        "20.1", "20.2", "20.3", "20.4", "20.5", "20.6", "20.7",
        "52.1", "52.2", "52.3",
    ]),
    ("Constitution (Consti)", [
        "21.1", "21.2", "21.3", "21.4",
        "72.1", "72.2", "72.3", "72.4",
    ]),

    # ── PART K: GROUPS ──
    ("Group Trust Layer", [
        "26.1", "26.2", "26.3", "26.4", "26.5",
    ]),

    # ── PART L: NETWORKING ──
    ("P2P Networking", [
        "12.1", "12.2", "12.3",
        "22.1", "22.2", "22.3", "22.4", "22.5", "22.6", "22.7",
        "59.1", "59.2", "59.3", "59.4", "59.5", "59.6", "59.7",
    ]),

    # ── PART M: STORAGE ──
    ("Storage, LMDB & Indexing", [
        "13.1", "13.2",
        "41.1", "41.2", "41.3", "41.4",
    ]),

    # ── PART N: NODE ──
    ("Node Operations", [
        "29.1", "29.2", "29.3", "29.4",
        "58.1", "58.2", "58.3", "58.4",
        "15.1", "54.1", "54.2", "54.3",
    ]),
    ("Error Handling & Recovery", [
        "33.1", "33.2", "33.3", "33.4", "33.5",
    ]),
    ("Bootstrapping & Network Launch", [
        "34.1", "34.2", "34.3", "34.4", "34.5",
    ]),

    # ── PART O: APIS ──
    ("RPC API", [
        "37.1", "37.2", "37.3", "37.4", "37.5",
    ]),
    ("WebSocket API", [
        "38.1", "38.2", "38.3",
    ]),
    ("Block Explorer & Tooling", [
        "65.1", "65.2", "65.3",
    ]),

    # ── PART P: WALLET ──
    ("Wallet", [
        "14.1", "14.2",
        "25.1", "25.2", "25.3", "25.4",
        "39.1", "39.2",
        "40.1", "40.2",
        "49.1", "49.2", "49.3",
    ]),

    # ── PART Q: UX ──
    ("Display & UX", [
        "31.1", "31.2", "31.3", "31.4", "31.5", "31.6",
        "42.1", "42.2",
    ]),

    # ── PART R: ECONOMICS & SECURITY ──
    ("Economics, Incentives & Privacy", [
        "28.1", "28.2", "28.3", "28.4",
        "50.1", "50.2", "50.3", "50.4",
        # 16.1 (privacy) REMOVED — duplicate of 51.1
        "51.1", "51.2", "51.3", "51.4",
        "53.1", "53.2",
    ]),

    # ── PART S: BUILD & DEPLOY ──
    ("Build, Test & Deploy", [
        "15.2",
        "63.1", "63.2", "63.3", "63.4", "63.5",
        "15.3", "15.4",
        "64.1", "64.2", "64.3", "64.4", "64.5",
        "55.1", "55.2", "55.3",
    ]),

    # ── PART T: EDGE CASES ──
    ("Catastrophic Scenarios", [
        "74.1", "74.2", "74.3", "74.4",
    ]),
    ("Open Design Questions", [
        "16.2", "16.3", "16.4",
    ]),

    # ── APPENDIX ──
    ("Comprehensive Parameter Defaults", [
        "66.1",
    ]),
]

# Questions intentionally removed (duplicates)
REMOVED = {"16.1"}  # duplicate of 51.1

# ─── Step 3: Generate ───

def generate(blocks: dict[str, dict]) -> str:
    out = [
        "# BURST Implementation Decisions",
        "",
        "Fill in your answers below each question. Where a default is suggested "
        "in **bold**, you can just write \"default\" or leave blank to accept it.",
        "",
    ]

    all_mapped: set[str] = set()
    for _title, qids in STRUCTURE:
        all_mapped.update(qids)

    # Warn about unmapped questions
    for old_id in sorted(blocks.keys(), key=lambda x: (int(x.split(".")[0]), int(x.split(".")[1]))):
        if old_id not in all_mapped and old_id not in REMOVED:
            print(f"  WARNING: question {old_id} ({blocks[old_id]['title']}) is not mapped!")

    for sec_idx, (sec_title, qids) in enumerate(STRUCTURE, 1):
        out.append("---")
        out.append("")
        out.append(f"## {sec_idx}. {sec_title}")
        out.append("")

        for q_idx, old_id in enumerate(qids, 1):
            if old_id not in blocks:
                print(f"  ERROR: {old_id} not found in source!")
                continue
            b = blocks[old_id]
            new_id = f"{sec_idx}.{q_idx}"
            out.append(f"### {new_id} {b['title']}")
            out.append(b["body"])
            out.append("")

    out.append("---")
    out.append("")
    out.append(
        "**Instructions**: Write your answer below each question, or write "
        "\"default\" to accept the bolded suggestion. If you disagree with any "
        "of my suggested defaults, just override them. Leave blank = accept default."
    )
    out.append("")
    out.append("When you're done, tell me and I'll start implementing.")
    out.append("")

    return "\n".join(out)


def main():
    text = SRC.read_text(encoding="utf-8")
    blocks = parse_raw(text)
    print(f"Parsed {len(blocks)} questions from source.")

    # Count mapped
    mapped = sum(len(qids) for _, qids in STRUCTURE)
    print(f"Mapped {mapped} questions into {len(STRUCTURE)} sections.")
    print(f"Removed {len(REMOVED)} duplicate(s): {REMOVED}")

    result = generate(blocks)

    # Validate
    new_q_count = result.count("\n### ")
    print(f"Output has {new_q_count} questions.")

    DST.write_text(result, encoding="utf-8")
    print(f"Written to {DST}")


if __name__ == "__main__":
    main()
