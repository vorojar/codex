"""Shared package metadata for Codex release packaging scripts."""

# `npm_name` is the local optional-dependency alias consumed by `bin/codex.js`.
# The underlying package published to npm is always `@openai/codex`.
CODEX_PLATFORM_PACKAGES: dict[str, dict[str, str]] = {
    "codex-linux-x64": {
        "npm_name": "@openai/codex-linux-x64",
        "npm_tag": "linux-x64",
        "target_triple": "x86_64-unknown-linux-musl",
        "os": "linux",
        "cpu": "x64",
    },
    "codex-linux-arm64": {
        "npm_name": "@openai/codex-linux-arm64",
        "npm_tag": "linux-arm64",
        "target_triple": "aarch64-unknown-linux-musl",
        "os": "linux",
        "cpu": "arm64",
    },
    "codex-darwin-x64": {
        "npm_name": "@openai/codex-darwin-x64",
        "npm_tag": "darwin-x64",
        "target_triple": "x86_64-apple-darwin",
        "os": "darwin",
        "cpu": "x64",
    },
    "codex-darwin-arm64": {
        "npm_name": "@openai/codex-darwin-arm64",
        "npm_tag": "darwin-arm64",
        "target_triple": "aarch64-apple-darwin",
        "os": "darwin",
        "cpu": "arm64",
    },
    "codex-win32-x64": {
        "npm_name": "@openai/codex-win32-x64",
        "npm_tag": "win32-x64",
        "target_triple": "x86_64-pc-windows-msvc",
        "os": "win32",
        "cpu": "x64",
    },
    "codex-win32-arm64": {
        "npm_name": "@openai/codex-win32-arm64",
        "npm_tag": "win32-arm64",
        "target_triple": "aarch64-pc-windows-msvc",
        "os": "win32",
        "cpu": "arm64",
    },
}

PACKAGE_EXPANSIONS: dict[str, list[str]] = {
    "codex": ["codex", *CODEX_PLATFORM_PACKAGES],
}

PACKAGE_NATIVE_COMPONENTS: dict[str, list[str]] = {
    "codex": [],
    "codex-linux-x64": ["codex", "rg"],
    "codex-linux-arm64": ["codex", "rg"],
    "codex-darwin-x64": ["codex", "rg"],
    "codex-darwin-arm64": ["codex", "rg"],
    "codex-win32-x64": ["codex", "rg", "codex-windows-sandbox-setup", "codex-command-runner"],
    "codex-win32-arm64": ["codex", "rg", "codex-windows-sandbox-setup", "codex-command-runner"],
    "codex-responses-api-proxy": ["codex-responses-api-proxy"],
    "codex-sdk": [],
}

PACKAGE_TARGET_FILTERS: dict[str, str] = {
    package_name: package_config["target_triple"]
    for package_name, package_config in CODEX_PLATFORM_PACKAGES.items()
}

PACKAGE_CHOICES = tuple(PACKAGE_NATIVE_COMPONENTS)
