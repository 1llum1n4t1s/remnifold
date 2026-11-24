# Create or update a server

Provide a Cloudflare API token through `CLOUDFLARE_API_TOKEN` or `--token-stdin`, then preview the existing resources:

```sh
remnifold server-setup --account-id <Cloudflare-account-id>
```

The default is authenticated, read-only discovery. The token needs account read, Workers scripts, D1 and Vectorize permissions; domain or route changes also need the corresponding Zone, Routes or Custom Domain permissions. Wrangler OAuth credentials are not imported.

For an existing installation, pass `--config <absolute-existing-jsonc-path>` to read its configuration without overwriting it. Resource names are reused in the order `remnifold`, `kagayoi-memory`, then `cloudflare-supermemory`; explicit `--worker-name`, `--database-name` and `--vector-index-name` take priority. Check that the plan selects your existing resources. IDs and vector dimensions/metric must match. Existing bindings, schedules and routes are preserved; conflicts stop setup. Do not create a replacement server for an upgrade.

Provide the memory key through `REMNIFOLD_API_KEY` and apply the plan with the same configuration and names:

```sh
remnifold server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

`--apply` creates missing resources, installs a supplied key as the Worker secret, applies additive D1 migrations, deploys the embedded Worker and checks the endpoint. For an existing server, supply its existing key to avoid replacing it. Without a key, setup retains the secret and reports authenticated verification as skipped; health alone does not verify migrations and MCP access. A supplied key verifies the session, remote MCP initialization and required tools.

Apply the server update and migrations before connecting or updating clients. Resumable state is stored outside the repository as `server/config.json` in the [configuration folder](configuration.md#connection-and-state); `--state <absolute-json-path>` selects another external location. Keep it, never commit generated IDs or credentials, and rerun the same command after a failed step. Use `--base-url` for an existing custom endpoint and `--help` for all options. Cloudflare bills usage to the hosting account.

Migrations retain original records and IDs. Migration `0011_consolidation_recall_opt_out.sql` (1.0.21) invalidates checkpoints containing recall-excluded sources; they can regenerate from eligible sources.
