#!/usr/bin/env python3

"""Verify codex-tui stays behind the app-server/core boundary."""

from __future__ import annotations

from dataclasses import dataclass
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
CODEX_PROTOCOL_GLOB_MESSAGE = "glob-imports `codex_protocol`, which exposes `protocol`"
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
EXTERN_CRATE_ALIAS_PATTERN = re.compile(
    rf"\bextern{REQUIRED_TOKEN_SEPARATOR}crate{REQUIRED_TOKEN_SEPARATOR}"
    rf"({IDENTIFIER}){REQUIRED_TOKEN_SEPARATOR}as{REQUIRED_TOKEN_SEPARATOR}"
    rf"({IDENTIFIER}){TOKEN_SEPARATOR};"
)


@dataclass(frozen=True)
class UseStatement:
    start: int
    tree_start: int
    tree: str


@dataclass(frozen=True)
class ModuleBlock:
    path: str
    open_index: int
    close_index: int


@dataclass(frozen=True)
class ImportAlias:
    module_path: str
    source: str
    alias: str


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
                sections.append((f"target.{target_name}.{section_name}", dependencies))

    return sections


def source_failures() -> list[str]:
    failures = []
    tui_manifest = tomllib.loads(TUI_MANIFEST.read_text())
    workspace_manifest = tomllib.loads(WORKSPACE_MANIFEST.read_text())
    codex_protocol_names = protocol_dependency_names(
        tui_manifest, workspace_dependencies(workspace_manifest)
    )
    source_texts = [
        (path, path.read_text()) for path in sorted(TUI_ROOT.glob("**/*.rs"))
    ]
    source_match_texts = [
        (path, non_code_as_whitespace(text)) for path, text in source_texts
    ]
    codex_protocol_aliases = collect_protocol_aliases(
        source_match_texts, codex_protocol_names
    )

    for (path, text), (_match_path, match_text) in zip(
        source_texts, source_match_texts, strict=True
    ):
        base_module_path = source_module_path(path)
        module_blocks = module_block_spans(match_text, base_module_path)
        seen_locations = set()
        for message, patterns in FORBIDDEN_SOURCE_RULES:
            for pattern in patterns:
                for match in pattern.finditer(match_text):
                    failures.append(source_failure(path, text, match.start(), message))
                    seen_locations.add((match.start(), message))

        for offset in protocol_reference_offsets(
            match_text, codex_protocol_aliases, base_module_path, module_blocks
        ):
            key = (offset, CODEX_PROTOCOL_MESSAGE)
            if key in seen_locations:
                continue
            failures.append(source_failure(path, text, offset, CODEX_PROTOCOL_MESSAGE))
            seen_locations.add(key)
        for offset in protocol_glob_import_offsets(
            match_text, codex_protocol_aliases, base_module_path, module_blocks
        ):
            key = (offset, CODEX_PROTOCOL_GLOB_MESSAGE)
            if key in seen_locations:
                continue
            failures.append(
                source_failure(path, text, offset, CODEX_PROTOCOL_GLOB_MESSAGE)
            )
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


def protocol_reference_offsets(
    text: str,
    codex_protocol_aliases: set[str],
    base_module_path: str,
    module_blocks: list[ModuleBlock],
) -> list[int]:
    offsets = []
    path_segment = rf"(?:self|super|crate|{IDENTIFIER})"
    pattern = re.compile(
        rf"\b((?:{path_segment}{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR})+)"
        rf"{PROTOCOL_IDENTIFIER}\b"
    )
    for match in pattern.finditer(text):
        module_path = module_path_at_offset(
            base_module_path, module_blocks, match.start()
        )
        if use_path_matches_alias(match.group(1), codex_protocol_aliases, module_path):
            offsets.append(match.start())
    offsets.extend(
        protocol_grouped_import_offsets(
            text, codex_protocol_aliases, base_module_path, module_blocks
        )
    )
    return offsets


def protocol_glob_import_offsets(
    text: str,
    codex_protocol_aliases: set[str],
    base_module_path: str,
    module_blocks: list[ModuleBlock],
) -> list[int]:
    offsets = []
    for statement in use_statements(text):
        module_path = module_path_at_offset(
            base_module_path, module_blocks, statement.start
        )
        if use_tree_imports_root_glob(
            statement.tree, codex_protocol_aliases, module_path
        ):
            offsets.append(statement.start)
    return offsets


def protocol_grouped_import_offsets(
    text: str,
    codex_protocol_aliases: set[str],
    base_module_path: str,
    module_blocks: list[ModuleBlock],
) -> list[int]:
    offsets = []
    for statement in use_statements(text):
        module_path = module_path_at_offset(
            base_module_path, module_blocks, statement.start
        )
        if use_tree_imports_protocol_at_root(
            statement.tree, codex_protocol_aliases, module_path
        ):
            offsets.append(statement.start)
    return offsets


def collect_protocol_aliases(
    source_texts: list[tuple[Path, str]], codex_protocol_names: set[str]
) -> set[str]:
    aliases = {normalize_path(crate_name) for crate_name in codex_protocol_names}
    import_aliases = all_import_aliases(source_texts)
    while True:
        previous_count = len(aliases)
        for import_alias in import_aliases:
            aliases.update(derived_protocol_aliases(import_alias, aliases))
        if len(aliases) == previous_count:
            return aliases


def all_import_aliases(source_texts: list[tuple[Path, str]]) -> list[ImportAlias]:
    aliases = []
    for path, text in source_texts:
        base_module_path = source_module_path(path)
        module_blocks = module_block_spans(text, base_module_path)
        for match in EXTERN_CRATE_ALIAS_PATTERN.finditer(text):
            module_path = module_path_at_offset(
                base_module_path, module_blocks, match.start()
            )
            aliases.append(
                ImportAlias(
                    module_path,
                    normalize_identifier(match.group(1)),
                    normalize_identifier(match.group(2)),
                )
            )
        for statement in use_statements(text):
            module_path = module_path_at_offset(
                base_module_path, module_blocks, statement.start
            )
            for source, alias in use_tree_import_aliases(statement.tree):
                aliases.append(ImportAlias(module_path, source, alias))
    return aliases


def derived_protocol_aliases(import_alias: ImportAlias, aliases: set[str]) -> set[str]:
    derived = set()
    alias_path = join_paths(import_alias.module_path, import_alias.alias)
    for source in path_candidates(import_alias.source, import_alias.module_path):
        if source in aliases:
            derived.add(alias_path)
        source_prefix = f"{source}::"
        for known_alias in aliases:
            if known_alias.startswith(source_prefix):
                suffix = known_alias.removeprefix(source_prefix)
                derived.add(join_paths(alias_path, suffix))
    return derived


def source_module_path(path: Path) -> str:
    try:
        relative = path.relative_to(TUI_ROOT / "src")
    except ValueError:
        return ""
    if relative.name in ("lib.rs", "main.rs"):
        return ""
    if relative.name == "mod.rs":
        parts = relative.parent.parts
    else:
        parts = (*relative.parent.parts, relative.stem)
    return "::".join(parts)


def module_block_spans(text: str, base_module_path: str) -> list[ModuleBlock]:
    spans = []
    pattern = re.compile(
        rf"\bmod{REQUIRED_TOKEN_SEPARATOR}({IDENTIFIER}){TOKEN_SEPARATOR}\{{"
    )
    for match in pattern.finditer(text):
        open_index = text.find("{", match.start(), match.end())
        close_index = matching_brace_index(text, open_index)
        if close_index is None:
            continue
        parent_path = module_path_at_offset(base_module_path, spans, match.start())
        module_name = normalize_identifier(match.group(1))
        spans.append(
            ModuleBlock(join_paths(parent_path, module_name), open_index, close_index)
        )
    return spans


def module_path_at_offset(
    base_module_path: str, module_blocks: list[ModuleBlock], offset: int
) -> str:
    module_path = base_module_path
    for module_block in module_blocks:
        if module_block.open_index < offset < module_block.close_index:
            module_path = module_block.path
    return module_path


def normalize_identifier(identifier: str) -> str:
    return identifier.removeprefix("r#")


def normalize_path(path: str) -> str:
    parts = [
        normalize_identifier(part)
        for part in re.split(rf"{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}", path.strip())
        if part
    ]
    return "::".join(parts)


def use_statements(text: str) -> list[UseStatement]:
    statements = []
    for match in re.finditer(r"\buse\b", text):
        index = match.end()
        while index < len(text) and text[index].isspace():
            index += 1
        tree_start = index
        depth = 0
        while index < len(text):
            char = text[index]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
            elif char == ";" and depth == 0:
                statements.append(
                    UseStatement(match.start(), tree_start, text[tree_start:index])
                )
                break
            index += 1
    return statements


def use_tree_import_aliases(tree: str) -> list[tuple[str, str]]:
    tree = tree.strip()
    root_body = root_braced_body(tree)
    if root_body is not None:
        body, _body_offset = root_body
        pairs = []
        for item, _offset in split_root_items(body):
            pairs.extend(use_tree_import_aliases(item))
        return pairs

    grouped = grouped_use_tree(tree)
    if grouped is not None:
        group_source, body, _body_offset = grouped
        pairs = []
        for item, _offset in split_root_items(body):
            source, alias = import_alias(item)
            if alias is not None:
                if source == "self":
                    if alias == "self":
                        alias = group_source.rsplit("::", 1)[-1]
                    pairs.append((group_source, alias))
                else:
                    pairs.append((join_paths(group_source, source), alias))
                continue
            pairs.extend(use_tree_import_aliases(join_paths(group_source, item)))
        return pairs

    source, alias = import_alias(tree)
    if alias is None:
        return []
    return [(source, alias)]


def import_alias(item: str) -> tuple[str, str | None]:
    match = re.fullmatch(
        rf"(?:::{TOKEN_SEPARATOR})?({PATH_PREFIX}{IDENTIFIER}|self)"
        rf"(?:{REQUIRED_TOKEN_SEPARATOR}as{REQUIRED_TOKEN_SEPARATOR}"
        rf"({IDENTIFIER}))?",
        item.strip(),
    )
    if match is None:
        return "", None

    source = normalize_path(match.group(1))
    explicit_alias = match.group(2)
    if explicit_alias is not None:
        alias = normalize_identifier(explicit_alias)
    elif source == "self":
        alias = "self"
    else:
        alias = source.rsplit("::", 1)[-1]
    return source, alias


def use_tree_imports_root_glob(tree: str, aliases: set[str], module_path: str) -> bool:
    tree = tree.strip()
    root_body = root_braced_body(tree)
    if root_body is not None:
        body, _body_offset = root_body
        return any(
            use_tree_imports_root_glob(item, aliases, module_path)
            for item, _ in split_root_items(body)
        )

    direct_glob_match = re.fullmatch(
        rf"(?:::{TOKEN_SEPARATOR})?({PATH_PREFIX}{IDENTIFIER})"
        rf"{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}\*",
        tree,
    )
    if direct_glob_match:
        return use_path_matches_alias(
            normalize_path(direct_glob_match.group(1)), aliases, module_path
        )

    grouped = grouped_use_tree(tree)
    if grouped is None:
        return False
    group_source, body, _body_offset = grouped
    items = split_root_items(body)
    if use_path_matches_alias(group_source, aliases, module_path) and any(
        item_without_alias(item).strip() == "*" for item, _ in items
    ):
        return True
    return any(
        use_tree_imports_root_glob(join_paths(group_source, item), aliases, module_path)
        for item, _ in items
    )


def use_tree_imports_protocol_at_root(
    tree: str, aliases: set[str], module_path: str
) -> bool:
    tree = tree.strip()
    root_body = root_braced_body(tree)
    if root_body is not None:
        body, _body_offset = root_body
        return any(
            use_tree_imports_protocol_at_root(item, aliases, module_path)
            for item, _ in split_root_items(body)
        )

    grouped = grouped_use_tree(tree)
    if grouped is None:
        return False
    group_source, body, _body_offset = grouped
    items = split_root_items(body)
    if use_path_matches_alias(group_source, aliases, module_path) and any(
        first_path_segment(item_without_alias(item)) == "protocol" for item, _ in items
    ):
        return True
    return any(
        use_tree_imports_protocol_at_root(
            join_paths(group_source, item), aliases, module_path
        )
        for item, _ in items
    )


def grouped_use_tree(tree: str) -> tuple[str, str, int] | None:
    brace_index = first_top_level_brace_index(tree)
    if brace_index is None:
        return None
    prefix = tree[:brace_index].strip()
    if not re.search(rf"::{TOKEN_SEPARATOR}$", prefix):
        return None
    close_index = matching_brace_index(tree, brace_index)
    if close_index is None or tree[close_index + 1 :].strip():
        return None
    group_source = normalize_path(
        re.sub(rf"{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}$", "", prefix)
    )
    return group_source, tree[brace_index + 1 : close_index], brace_index + 1


def root_braced_body(tree: str) -> tuple[str, int] | None:
    tree = tree.strip()
    if tree.startswith("::"):
        tree = tree[2:].strip()
    if not tree.startswith("{"):
        return None
    close_index = matching_brace_index(tree, 0)
    if close_index is None or tree[close_index + 1 :].strip():
        return None
    return tree[1:close_index], 1


def first_top_level_brace_index(text: str) -> int | None:
    depth = 0
    for index, char in enumerate(text):
        if char == "{":
            if depth == 0:
                return index
            depth += 1
        elif char == "}":
            depth -= 1
    return None


def matching_brace_index(text: str, open_index: int) -> int | None:
    depth = 0
    for index in range(open_index, len(text)):
        char = text[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return index
    return None


def split_root_items(body: str) -> list[tuple[str, int]]:
    items = []
    depth = 0
    item_start = 0
    for index, char in enumerate(body):
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == "," and depth == 0:
            append_root_item(items, body, item_start, index)
            item_start = index + 1
    append_root_item(items, body, item_start, len(body))
    return items


def append_root_item(
    items: list[tuple[str, int]], body: str, start: int, end: int
) -> None:
    item = body[start:end]
    leading = len(item) - len(item.lstrip())
    item = item.strip()
    if item:
        items.append((item, start + leading))


def item_alias(item: str) -> tuple[str, str | None]:
    match = re.fullmatch(
        rf"({PATH_PREFIX}{IDENTIFIER}|self)"
        rf"{REQUIRED_TOKEN_SEPARATOR}as{REQUIRED_TOKEN_SEPARATOR}"
        rf"({IDENTIFIER})",
        item.strip(),
    )
    if match is None:
        return "", None
    return normalize_path(match.group(1)), normalize_identifier(match.group(2))


def join_paths(prefix: str, suffix: str) -> str:
    if not prefix:
        return suffix
    if not suffix:
        return prefix
    return f"{prefix}::{suffix}"


def use_path_matches_alias(path: str, aliases: set[str], module_path: str) -> bool:
    return any(candidate in aliases for candidate in path_candidates(path, module_path))


def path_candidates(path: str, module_path: str) -> set[str]:
    normalized_path = normalize_path(path)
    if not normalized_path:
        return set()

    parts = normalized_path.split("::")
    if parts[0] == "crate":
        return {strip_root_qualifier(normalized_path)}
    if parts[0] == "self":
        return {join_paths(module_path, "::".join(parts[1:]))}
    if parts[0] == "super":
        parent_parts = module_path.split("::") if module_path else []
        path_parts = parts
        while path_parts and path_parts[0] == "super":
            if parent_parts:
                parent_parts.pop()
            path_parts = path_parts[1:]
        return {join_paths("::".join(parent_parts), "::".join(path_parts))}

    candidates = {normalized_path}
    if module_path:
        candidates.add(join_paths(module_path, normalized_path))
    return candidates


def strip_root_qualifier(path: str) -> str:
    parts = path.split("::")
    while parts and parts[0] in ("self", "crate", "super"):
        parts = parts[1:]
    return "::".join(parts)


def item_without_alias(item: str) -> str:
    return re.split(
        rf"\b{REQUIRED_TOKEN_SEPARATOR}as{REQUIRED_TOKEN_SEPARATOR}\b", item, 1
    )[0].strip()


def first_path_segment(path: str) -> str:
    return normalize_identifier(
        re.split(rf"{TOKEN_SEPARATOR}::{TOKEN_SEPARATOR}", path.strip(), maxsplit=1)[0]
    )


def source_failure(path: Path, text: str, offset: int, message: str) -> str:
    line_number = text.count("\n", 0, offset) + 1
    return f"{relative_path(path)}:{line_number} {message}"


def relative_path(path: Path) -> str:
    return str(path.relative_to(ROOT))


if __name__ == "__main__":
    sys.exit(main())
