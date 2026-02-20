#!/usr/bin/env python3
"""BURST Implementation Decisions — Interactive Questionnaire Server.

Usage:
    python server.py          # opens http://localhost:7890
    python server.py --port 8080
"""

import json
import re
import sys
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
from urllib.parse import urlparse

DECISIONS_PATH = Path(__file__).resolve().parent.parent / "IMPLEMENTATION_DECISIONS.md"

# ──────────────────────────── Parser ────────────────────────────

STRUCTURAL_RE = re.compile(
    r"^(### \d+\.\d+|## \d+\.|---$|If \([a-z]\)|Your answers?\b|\*\*Instructions)"
)

def parse_markdown() -> list[dict]:
    text = DECISIONS_PATH.read_text(encoding="utf-8")
    sections: list[dict] = []
    cur_sec = None
    cur_q = None
    answer_buf: list[str] = []
    body_buf: list[str] = []
    collecting_answer = False

    def flush_question():
        nonlocal cur_q, body_buf, answer_buf, collecting_answer
        if cur_q is None:
            return
        if collecting_answer:
            cur_q["answers"].append("\n".join(answer_buf).strip())
            answer_buf.clear()
            collecting_answer = False
        _process_body(cur_q, body_buf)
        body_buf = []

    for raw_line in text.split("\n"):
        line = raw_line

        # ── section header ──
        m = re.match(r"^## (\d+)\. (.+)", line)
        if m:
            flush_question()
            cur_sec = {"id": int(m.group(1)), "title": m.group(2).strip(), "questions": []}
            sections.append(cur_sec)
            cur_q = None
            continue

        # ── question header ──
        m = re.match(r"^### (\d+\.\d+) (.+)", line)
        if m:
            flush_question()
            cur_q = {
                "id": m.group(1),
                "title": m.group(2).strip(),
                "description": "",
                "options": [],
                "table": None,
                "answers": [],
                "sub_prompts": [],
            }
            if cur_sec:
                cur_sec["questions"].append(cur_q)
            collecting_answer = False
            body_buf = []
            answer_buf = []
            continue

        if cur_q is None:
            continue

        # ── answer marker ──
        if re.match(r"^Your answers?\b", line):
            if collecting_answer:
                cur_q["answers"].append("\n".join(answer_buf).strip())
                answer_buf.clear()
            collecting_answer = True
            continue

        if collecting_answer:
            # Section divider ends the answer
            if line.strip() == "---":
                cur_q["answers"].append("\n".join(answer_buf).strip())
                answer_buf.clear()
                collecting_answer = False
                continue
            # Sub-prompt between answer slots
            sm = re.match(r"^If \(([a-z])\),?\s*(.+)", line)
            if sm:
                cur_q["answers"].append("\n".join(answer_buf).strip())
                answer_buf.clear()
                collecting_answer = False
                cur_q["sub_prompts"].append(line)
                body_buf.append(line)
                continue
            answer_buf.append(line)
        else:
            body_buf.append(line)

    flush_question()
    return sections


def _process_body(question: dict, lines: list[str]):
    desc_parts: list[str] = []
    options: list[dict] = []
    table_headers = None
    table_rows: list[list[str]] = []

    for line in lines:
        # default option (bold)
        m = re.match(r"^- \*\*\(([a-zA-Z])\)\s*(.+?)\*\*(.*)", line)
        if m:
            txt = m.group(2).strip()
            extra = m.group(3).strip()
            if extra:
                txt += " " + extra
            options.append({"id": m.group(1).lower(), "text": txt, "default": True})
            continue

        # non-default option
        m = re.match(r"^- \(([a-zA-Z])\)\s*(.+)", line)
        if m:
            options.append({"id": m.group(1).lower(), "text": m.group(2).strip(), "default": False})
            continue

        # table row
        if line.strip().startswith("|"):
            cells = [c.strip() for c in line.strip().split("|")[1:-1]]
            if all(re.match(r"^[-:]+$", c) for c in cells):
                continue
            if table_headers is None:
                table_headers = cells
            else:
                table_rows.append(cells)
            continue

        if line.strip():
            desc_parts.append(line)

    question["description"] = "\n".join(desc_parts).strip()
    question["options"] = options
    if table_headers:
        question["table"] = {"headers": table_headers, "rows": table_rows}


# ──────────────────────────── Saver ─────────────────────────────

def save_answers(answers: dict[str, str]):
    text = DECISIONS_PATH.read_text(encoding="utf-8")
    lines = text.split("\n")
    result: list[str] = []
    cur_qid = None
    answer_idx = 0
    i = 0

    while i < len(lines):
        line = lines[i]

        m = re.match(r"^### (\d+\.\d+)", line)
        if m:
            cur_qid = m.group(1)
            answer_idx = 0

        if re.match(r"^Your answers?\b", line):
            result.append(line)
            i += 1

            # skip old answer lines
            while i < len(lines):
                peek = lines[i]
                if (
                    peek.startswith("### ")
                    or peek.startswith("## ")
                    or peek.startswith("---")
                    or re.match(r"^If \([a-z]\)", peek)
                    or re.match(r"^Your answers?\b", peek)
                    or (peek.startswith("|") and "|" in peek[1:])
                    or peek.startswith("**")
                ):
                    break
                i += 1

            key = f"{cur_qid}_{answer_idx}" if cur_qid else None
            if key and key in answers and answers[key].strip():
                result.append(answers[key])
            result.append("")
            answer_idx += 1
            continue

        result.append(line)
        i += 1

    DECISIONS_PATH.write_text("\n".join(result), encoding="utf-8")


# ──────────────────────────── Server ────────────────────────────

class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=str(Path(__file__).resolve().parent), **kw)

    def log_message(self, fmt, *args):
        pass  # silence per-request logs

    def _json_response(self, code: int, data):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(json.dumps(data).encode())

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/api/questions":
            self._json_response(200, parse_markdown())
            return
        if path == "/":
            self.path = "/index.html"
        super().do_GET()

    def do_POST(self):
        if self.path == "/api/save":
            body = self.rfile.read(int(self.headers["Content-Length"]))
            save_answers(json.loads(body))
            self._json_response(200, {"status": "saved"})
            return

    def do_OPTIONS(self):
        self.send_response(200)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()


def main():
    port = 7890
    for i, a in enumerate(sys.argv[1:]):
        if a == "--port" and i + 2 < len(sys.argv):
            port = int(sys.argv[i + 2])
    srv = HTTPServer(("127.0.0.1", port), Handler)
    print(f"\n  \033[1;35m⬡ BURST\033[0m Implementation Decisions Questionnaire")
    print(f"  \033[36m→ http://localhost:{port}\033[0m\n")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\n  Stopped.")


if __name__ == "__main__":
    main()
