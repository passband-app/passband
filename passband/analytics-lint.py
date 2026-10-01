#!/usr/bin/env python3
"""Analytics vocabulary lint.

Analytics.swift enforces a CLOSED vocabulary at runtime: an event name or a
string property value outside its sets trips an assert in a debug build and is
silently dropped in release. Nothing at compile time catches the omission, and
the miss has shipped three times (forward, shipments_poll_kicked,
shipment_cleared) because the failure only shows when the code path fires.

This is the compile-time check. It reads the sets straight out of
Analytics.swift and every capture site out of Sources/, and fails on:

  * an event captured anywhere that is not in `allowedEvents`
  * an `allowedEvents` name that no code path captures (dead vocabulary)
  * a `minimalEvents` name that is not also in `allowedEvents`
  * a string LITERAL used directly as a property value at a capture site that
    is not in `allowedStrings` (values that arrive through a `.rawValue` are
    the enum's business and are not checked here)

Run by test.sh, and cheap enough to run on every save.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent
ANALYTICS = ROOT / "Sources/Passband/Lib/Analytics.swift"
SOURCES = ROOT / "Sources"

# Event names whose capture lives inside Analytics.swift itself, or which
# PostHog owns: lifecycle events, screens and person sets.
INTERNAL = {"$screen", "$set", "Application Opened", "Application Backgrounded"}


def strip_comments(text: str) -> str:
    return re.sub(r"//[^\n]*", "", text)


def balanced(text: str, start: int, open_ch: str, close_ch: str) -> str:
    """The text between the bracket at `start` and its match, exclusive."""
    depth = 0
    for i in range(start, len(text)):
        if text[i] == open_ch:
            depth += 1
        elif text[i] == close_ch:
            depth -= 1
            if depth == 0:
                return text[start + 1 : i]
    sys.exit(f"analytics-lint: unbalanced {open_ch} at {start}")


LITERAL = re.compile(r'"([^"\n]+)"')


def literal_set(src: str, name: str) -> set[str]:
    """Every "…" literal inside the array literal that initializes `name`.
    `allowedStrings` is `Set(...).union([...])`; the bracket after `.union(`
    is the one that holds the vocabulary."""
    at = src.find(f"let {name}")
    if at < 0:
        sys.exit(f"analytics-lint: could not find {name} in {ANALYTICS}")
    union = src.find(".union(", at)
    eq = src.find("=", at)
    bracket = src.find("[", union if 0 <= union < src.find("\n\n", at) else eq)
    return set(LITERAL.findall(balanced(src, bracket, "[", "]")))


CALL = re.compile(r"(?:Analytics\.(?:capture|daily)|client\?\.capture)\(")
VALUE = re.compile(r'"[A-Za-z_$]+"\s*:\s*"([^"]+)"')


def first_argument(args: str) -> str:
    """The event-name argument: everything before the first top-level comma."""
    depth = 0
    for i, ch in enumerate(args):
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == "," and depth == 0:
            return args[:i]
    return args


def main() -> int:
    analytics = strip_comments(ANALYTICS.read_text())
    allowed = literal_set(analytics, "allowedEvents")
    minimal = literal_set(analytics, "minimalEvents")
    strings = literal_set(analytics, "allowedStrings")
    # MainView raw values are unioned in at runtime; mirror that here.
    app_store = strip_comments((SOURCES / "Passband/Model/AppStore.swift").read_text())
    mv = re.search(r"enum MainView[^{]*\{\s*case ([a-z, ]+)", app_store)
    if mv:
        strings |= {c.strip() for c in mv.group(1).split(",")}

    captured: dict[str, list[str]] = {}
    bad_strings: list[str] = []
    for path in SOURCES.rglob("*.swift"):
        text = strip_comments(path.read_text())
        rel = str(path.relative_to(ROOT))
        for m in CALL.finditer(text):
            args = balanced(text, m.end() - 1, "(", ")")
            # A ternary in the name position names two events; both count.
            head = first_argument(args)
            names = LITERAL.findall(head)
            for name in names:
                captured.setdefault(name, []).append(rel)
            # Values are checked in the PROPERTIES only: the name position
            # may hold a ternary, whose `? "a" : "b"` is not a key-value pair.
            for v in VALUE.findall(args[len(head):]):
                if v not in strings:
                    bad_strings.append(f'{rel}: "{v}" (in {"/".join(names)})')

    errors: list[str] = []
    for event, sites in sorted(captured.items()):
        if event.startswith("$") or event in INTERNAL:
            continue
        if event not in allowed:
            errors.append(
                f"captured but not in allowedEvents: {event} ({', '.join(sorted(set(sites)))})")
    for event in sorted(allowed - captured.keys() - INTERNAL):
        errors.append(f"in allowedEvents but never captured: {event}")
    for event in sorted(minimal - allowed):
        errors.append(f"in minimalEvents but not allowedEvents: {event}")
    errors += [f"string value not in allowedStrings: {b}" for b in bad_strings]

    if errors:
        print("analytics-lint: FAIL")
        for e in errors:
            print("  " + e)
        return 1
    print(f"analytics-lint: ok ({len(allowed)} events, {len(strings)} strings)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
