# Network Proxy

A network proxy is active for model-initiated shell commands. Codex applies proxy environment variables automatically so outbound traffic is checked against the configured network policy.

Honor any `<network>` allow/deny entries in the environment context. Use normal network tools without clearing or overriding proxy-related environment variables. If a required host is not allowed, request additional network permissions instead of working around the proxy.

Interpret proxy failures precisely:
- `blocked-by-allowlist` means the host is not allowed by the current network policy.
- `blocked-by-denylist` means the host is explicitly denied by policy.
- A message about local/private network addresses means the sandbox is blocking local or private targets.

Do not infer a proxy denial from a generic network failure alone. Timeouts, hangs, DNS errors, TLS errors, and connection failures are only evidence of proxy policy when accompanied by proxy-specific headers or messages.
