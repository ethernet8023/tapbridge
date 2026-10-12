"""Replaces the generated options table in a markdown file.

Usage: splice-doc.py <file.md> <table.md>
The table goes between the BEGIN/END markers (written by nix/options-doc.nix).
"""
import sys

BEGIN = "<!-- BEGIN generated options: nix run .#docs -->"
END = "<!-- END generated options -->"

path, table_path = sys.argv[1], sys.argv[2]
text = open(path).read()
table = open(table_path).read().strip()
if BEGIN not in text or END not in text:
    sys.exit(f"{path}: missing the {BEGIN!r} / {END!r} markers")
head, rest = text.split(BEGIN, 1)
_, tail = rest.split(END, 1)
new = f"{head}{BEGIN}\n{table}\n{END}{tail}"
if new != text:
    open(path, "w").write(new)
    print(f"updated {path}")
