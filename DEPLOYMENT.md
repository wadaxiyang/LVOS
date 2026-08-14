# LVOS Server deployment

LVOS Server production deployments are supported through Docker Compose. The stack contains only the Server and its persistent `lvos-data` volume. Put your own HTTPS reverse proxy in front of the loopback listener; TLS certificates and proxy configuration are intentionally not bundled.

## Configure and start

Docker Engine with Compose v2 is required. Create a local `.env` file and protect it as a secret-bearing file:

```dotenv
LVOS_PUBLIC_SERVER_URL=https://lvos.example.com
LVOS_DEFAULT_USERNAME=default
LVOS_DEFAULT_PASSWORD=replace-with-a-unique-long-password
```

The password must be unique and at least 12 characters. Production startup rejects a missing, example, or unsafe password. Then start the service:

```sh
docker compose up --build -d
docker compose ps
curl --fail http://127.0.0.1:7770/api/v1/health
```

The health JSON declares `server_api_version`, `server_version`, and
`minimum_desktop_version`. Desktop validates these fields before login or Session restoration and
will refuse incompatible Server work while keeping local data available.

The container health check uses the Server's own `healthcheck` command, so the runtime image does not need a shell health utility.

Expose the public hostname only through your HTTPS reverse proxy. The default Compose port binding is loopback-only. If the reverse proxy runs in another container, attach it to an operator-managed Docker network and route to `lvos-server:7770` without publishing the Server directly to the internet.

## Persistent data and backups

The `lvos-data` named volume contains the SQLite database and consistent backups. Do not use `docker compose down --volumes` unless you intend to delete both. The Server creates a periodic backup every 24 hours by default and retains 14 LVOS backups; override `LVOS_BACKUP_INTERVAL_HOURS` and `LVOS_BACKUP_RETENTION_COUNT` in `.env`.

Trigger a consistent backup without starting a second long-running Server:

```sh
docker compose exec lvos-server lvos-server backup
```

For recovery, stop the Server, identify a backup inside `/var/lib/lvos/backups`, run the one-shot restore, and then start again:

```sh
docker compose stop lvos-server
docker compose run --rm --no-deps lvos-server restore /var/lib/lvos/backups/BACKUP_FILE.sqlite3
docker compose start lvos-server
```

Restore verifies the selected backup and creates a `pre-restore` recovery backup before replacing the database. Keep an independent copy of the Docker volume for full disaster recovery.

## Build-source overrides

The default base images use the DaoCloud Docker Hub proxy and Cargo uses the RsProxy sparse index for mainland-China accessibility. These are build configuration, not protocol dependencies. To use official sources, set:

```dotenv
LVOS_DOCKER_RUST_IMAGE=rust:1.94.1-bookworm
LVOS_DOCKER_RUNTIME_IMAGE=debian:bookworm-slim
LVOS_CARGO_REGISTRY_INDEX=sparse+https://index.crates.io/
```

Mirror availability can change. Select sources appropriate for the deployment network and rebuild with `docker compose build --pull`.

The image build uses `Cargo.server.toml`, a deployment-only workspace that contains the Server and its actual library dependencies. Desktop UI dependencies are not part of the private Server image build.

## Updating

Take a manual backup, pull the new source or release, rebuild, and restart with Compose. Startup applies explicit migrations only after creating a consistent pre-migration backup. A migration failure rolls back the migration and refuses startup.

Before a production update, also run `docker compose config` and confirm the public URL, loopback
binding, persistent volume, and secret-bearing `.env` are the intended deployment values. After
restart, check health, log in from one Desktop, and confirm a second Device can catch up before
removing any independent pre-update backup.
