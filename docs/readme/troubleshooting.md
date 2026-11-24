# Troubleshooting

Capture failures report fixed `stage`, `cause` and `action` codes without exception text or credentials.

- `action=repair-required`, including `stage=receipt; cause=invalid`, requires repairing the corresponding local capture state. Retrying alone will not fix it.
- `action=retry-or-repair` keeps normal retries while you investigate a persistent connection or response problem.

Check the API separately:

```sh
remnifold status
```

Local capture state is in `capture-state/` in the [app state folder](configuration.md#connection-and-state). Receipts from older Codex-home locations remain readable.

Setup update diagnostics are in `app-update.log`. A registration that did not finish is retried at the next logon. For server setup failures, correct the cause and rerun the same command with its retained state; see [server setup](create-or-update-a-server.md).
