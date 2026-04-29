#!/usr/bin/env python3

"""Verify codex-tui stays behind the app-server/core boundary."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_MANIFEST = ROOT / "codex-rs" / "Cargo.toml"
TUI_ROOT = ROOT / "codex-rs" / "tui"
TUI_MANIFEST = TUI_ROOT / "Cargo.toml"
FORBIDDEN_PACKAGE = "codex-core"
CODEX_PROTOCOL_PACKAGE = "codex-protocol"
CODEX_PROTOCOL_MESSAGE = "references `codex_protocol::protocol`"
IDENTIFIER = r"(?:r#)?[^\W\d]\w*"
PROTOCOL_IDENTIFIER = r"(?:r#)?protocol"
TOKEN_SEPARATOR = r"\s*"
REQUIRED_TOKEN_SEPARATOR = r"\s+"
PATH_PREFIX = rf"(?:(?:{IDENTIFIER}){TOKEN_SEPARATOR}::{TOKEN_SEPARATOR})*"
FORBIDDEN_SOURCE_RULES = (
    (
        "imports `codex_core`",
        (
            re.compile(r"\bcodex_core::"),
            re.compile(r"\buse\s+codex_core\b"),
            re.compile(r"\bextern\s+crate\s+codex_core\b"),
        ),
    ),
)
CRATE_ALIAS_PATTERNS = (
    re.compile(
        rf"\buse{REQUIRED_TOKEN_SEPARATOR}"
        rf"(?:::{TOKEN_SEPARATOR})?({PATH_PREFIX}{IDENTIFIER})"
        rf"{REQUIRED_TOKEN_SEPARATOR}as{REQUIRED_TOKEN_SEPARATOR}"
        rf"({IDENTIFIER}){TOKEN_SEPARATOR};"
    ),
    re.compile(
        rf"\bextern{REQUIRED_TOKEN_SEPARATOR}crate{REQUIRED_TOKEN_SEPARATOR}"
        rf"({IDENTIFIER}){REQUIRED_TOKEN_SEPARATOR}as{REQUIRED_TOKEN_SEPARATOR}"
        rf"({IDENTIFIER}){TOKEN_SEPARATOR};"
    ),
)
GROUPED_USE_PATTERN = re.compile(
    rf"\buse{REQUIRED_TOKEN_SEPARATOR}"
    rf"(?:::{TOKEN_SEPARATOR})?({PATH_PREFIX}{IDENTIFIER})"
    rf"{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}\{{([^;]*)\}}\s*;"
)
GROUPED_SELF_ALIAS_PATTERN = re.compile(
    rf"\bself{REQUIRED_TOKEN_SEPARATOR}as{REQUIRED_TOKEN_SEPARATOR}({IDENTIFIER})\b"
)


def main() -> int:
    failures = []
    failures.extend(manifest_failures())
    failures.extend(source_failures())

    if not failures:
        return 0

    print("codex-tui must stay behind the app-server/core boundary.")
    print(
        "Use app-server protocol types at the TUI boundary; temporary embedded "
        "startup gaps belong behind codex_app_server_client::legacy_core, and "
        "core protocol references should remain outside codex-tui."
    )
    print()
    for failure in failures:
        print(f"- {failure}")

    return 1


def manifest_failures() -> list[str]:
    manifest = tomllib.loads(TUI_MANIFEST.read_text())
    failures = []
    for section_name, dependencies in dependency_sections(manifest):
        if FORBIDDEN_PACKAGE in dependencies:
            failures.append(
                f"{relative_path(TUI_MANIFEST)} declares `{FORBIDDEN_PACKAGE}` "
                f"in `[{section_name}]`"
            )
    return failures


def dependency_sections(manifest: dict) -> list[tuple[str, dict]]:
    sections: list[tuple[str, dict]] = []
    for section_name in ("dependencies", "dev-dependencies", "build-dependencies"):
        dependencies = manifest.get(section_name)
        if isinstance(dependencies, dict):
            sections.append((section_name, dependencies))

    for target_name, target in manifest.get("target", {}).items():
        if not isinstance(target, dict):
            continue
        for section_name in ("dependencies", "dev-dependencies", "build-dependencies"):
            dependencies = target.get(section_name)
            if isinstance(dependencies, dict):
                sections.append((f'target.{target_name}.{section_name}', dependencies))

    return sections


def source_failures() -> list[str]:
    failures = []
    tui_manifest = tomllib.loads(TUI_MANIFEST.read_text())
    workspace_manifest = tomllib.loads(WORKSPACE_MANIFEST.read_text())
    codex_protocol_names = protocol_dependency_names(
        tui_manifest, workspace_dependencies(workspace_manifest)
    )
    source_texts = [(path, path.read_text()) for path in sorted(TUI_ROOT.glob("**/*.rs"))]

    for path, text in source_texts:
        match_text = non_code_as_whitespace(text)
        seen_locations = set()
        for message, patterns in FORBIDDEN_SOURCE_RULES:
            for pattern in patterns:
                for match in pattern.finditer(match_text):
                    failures.append(source_failure(path, text, match.start(), message))
                    seen_locations.add((match.start(), message))

        for match in protocol_reference_matches(match_text, codex_protocol_names):
            key = (match.start(), CODEX_PROTOCOL_MESSAGE)
            if key in seen_locations:
                continue
            failures.append(source_failure(path, text, match.start(), CODEX_PROTOCOL_MESSAGE))
            seen_locations.add(key)
    return failures


def non_code_as_whitespace(text: str) -> str:
    chars = list(text)
    index = 0
    while index < len(text):
        if text.startswith("//", index):
            index = mask_line_comment(chars, index)
            continue
        if text.startswith("/*", index):
            index = mask_block_comment(chars, index)
            continue
        char_literal_end_index = char_literal_end(text, index)
        if char_literal_end_index is not None:
            mask_range(chars, index, char_literal_end_index)
            index = char_literal_end_index
            continue
        raw_string_end_index = raw_string_end(text, index)
        if raw_string_end_index is not None:
            mask_range(chars, index, raw_string_end_index)
            index = raw_string_end_index
            continue
        quoted_string_end_index = quoted_string_end(text, index)
        if quoted_string_end_index is not None:
            mask_range(chars, index, quoted_string_end_index)
            index = quoted_string_end_index
            continue
        index += 1
    return "".join(chars)


def mask_line_comment(chars: list[str], start: int) -> int:
    index = start
    while index < len(chars):
        original = chars[index]
        chars[index] = "\n" if original == "\n" else " "
        index += 1
        if original == "\n":
            break
    return index


def mask_block_comment(chars: list[str], start: int) -> int:
    text = "".join(chars)
    index = start
    depth = 0
    while index < len(chars):
        if text.startswith("/*", index):
            depth += 1
            mask_range(chars, index, index + 2)
            index += 2
            continue
        if text.startswith("*/", index):
            depth -= 1
            mask_range(chars, index, index + 2)
            index += 2
            if depth == 0:
                break
            continue
        chars[index] = "\n" if chars[index] == "\n" else " "
        index += 1
    return index


def char_literal_end(text: str, start: int) -> int | None:
    quote_start = None
    if text.startswith("'", start):
        quote_start = start
    elif text.startswith("b'", start):
        quote_start = start + 1
    if quote_start is None:
        return None

    index = quote_start + 1
    if index >= len(text) or text[index] == "\n":
        return None
    if text[index] == "\\":
        index = escaped_char_end(text, index)
    else:
        index += 1
    if index < len(text) and text[index] == "'":
        return index + 1
    return None


def escaped_char_end(text: str, start: int) -> int:
    index = start + 1
    if (
        index < len(text)
        and text[index] == "u"
        and index + 1 < len(text)
        and text[index + 1] == "{"
    ):
        closing_index = text.find("}", index + 2)
        if closing_index != -1:
            return closing_index + 1
    return min(start + 2, len(text))


def raw_string_end(text: str, start: int) -> int | None:
    raw_start = None
    if text.startswith(("br", "cr"), start):
        raw_start = start + 1
    elif text.startswith("r", start):
        raw_start = start
    if raw_start is None:
        return None

    index = raw_start + 1
    while index < len(text) and text[index] == "#":
        index += 1
    if index >= len(text) or text[index] != '"':
        return None

    closing = '"' + "#" * (index - raw_start - 1)
    closing_index = text.find(closing, index + 1)
    if closing_index == -1:
        return len(text)
    return closing_index + len(closing)


def quoted_string_end(text: str, start: int) -> int | None:
    quote_start = None
    if text.startswith(('"', 'b"', 'c"'), start):
        quote_start = start if text[start] == '"' else start + 1
    if quote_start is None:
        return None

    index = quote_start + 1
    while index < len(text):
        if text[index] == "\\":
            index += 2
            continue
        if text[index] == '"':
            return index + 1
        index += 1
    return len(text)


def mask_range(chars: list[str], start: int, end: int) -> None:
    for index in range(start, min(end, len(chars))):
        chars[index] = "\n" if chars[index] == "\n" else " "


def workspace_dependencies(manifest: dict) -> dict:
    dependencies = manifest.get("workspace", {}).get("dependencies", {})
    if isinstance(dependencies, dict):
        return dependencies
    return {}


def protocol_dependency_names(manifest: dict, workspace_dependencies: dict) -> set[str]:
    names = {"codex_protocol"}
    for _section_name, dependencies in dependency_sections(manifest):
        for dependency_name, dependency_value in dependencies.items():
            package_name = dependency_package_name(
                dependency_name, dependency_value, workspace_dependencies
            )
            if package_name == CODEX_PROTOCOL_PACKAGE:
                names.add(rust_crate_name(dependency_name))
    return names


def dependency_package_name(
    dependency_name: str, dependency_value: object, workspace_dependencies: dict
) -> str:
    if isinstance(dependency_value, dict):
        if "package" in dependency_value:
            return dependency_value["package"]
        if dependency_value.get("workspace") is True:
            workspace_dependency = workspace_dependencies.get(dependency_name)
            if isinstance(workspace_dependency, dict):
                return workspace_dependency.get("package", dependency_name)
    return dependency_name


def rust_crate_name(package_or_dependency_name: str) -> str:
    return package_or_dependency_name.replace("-", "_")


def protocol_reference_matches(
    text: str, codex_protocol_names: set[str]
) -> list[re.Match[str]]:
    matches = []
    for crate_name in expanded_crate_aliases(text, codex_protocol_names):
        crate_name_pattern = rf"(?:r#)?{re.escape(normalize_identifier(crate_name))}"
        patterns = (
            re.compile(
                rf"\b{crate_name_pattern}{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}"
                rf"{PROTOCOL_IDENTIFIER}\b"
            ),
            re.compile(
                rf"\b{crate_name_pattern}{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}"
                rf"\{{[^;]*\b{PROTOCOL_IDENTIFIER}\b"
            ),
        )
        for pattern in patterns:
            matches.extend(pattern.finditer(text))
    return matches


def expanded_crate_aliases(text: str, crate_names: set[str]) -> set[str]:
    aliases = {normalize_identifier(crate_name) for crate_name in crate_names}
    while True:
        previous_count = len(aliases)
        for source, alias in crate_alias_pairs(text):
            if source in aliases:
                aliases.add(alias)
        if len(aliases) == previous_count:
            return aliases


def crate_alias_pairs(text: str) -> list[tuple[str, str]]:
    pairs = []
    for pattern in CRATE_ALIAS_PATTERNS:
        for match in pattern.finditer(text):
            pairs.append(
                (
                    normalize_identifier(path_tail(match.group(1))),
                    normalize_identifier(match.group(2)),
                )
            )
    for match in GROUPED_USE_PATTERN.finditer(text):
        source = normalize_identifier(path_tail(match.group(1)))
        body = match.group(2)
        for alias_match in GROUPED_SELF_ALIAS_PATTERN.finditer(body):
            pairs.append((source, normalize_identifier(alias_match.group(1))))
    return pairs


def normalize_identifier(identifier: str) -> str:
    return identifier.removeprefix("r#")


def path_tail(path: str) -> str:
    return re.split(rf"{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}", path)[-1]


def source_failure(path: Path, text: str, offset: int, message: str) -> str:
    line_number = text.count("\n", 0, offset) + 1
    return f"{relative_path(path)}:{line_number} {message}"


def relative_path(path: Path) -> str:
    return str(path.relative_to(ROOT))


if __name__ == "__main__":
    sys.exit(main())
