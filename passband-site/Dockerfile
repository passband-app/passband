# Landing page. The Railway service's root directory is passband-site, so this
# builds with the site directory as its context and the root railway.toml
# (relay config, see deploy/DEPLOY.md §8) never applies to this service.
FROM oven/bun:1-slim@sha256:1846adf22496bd711036aae8be147e57bdebbaea2e5264e3e0b5af1db187d9a4
WORKDIR /app

COPY package.json bun.lockb ./
RUN bun install --frozen-lockfile

COPY . .
RUN bun run build

ENV NODE_ENV=production
# The base image ships an unprivileged `bun` user. The server only reads the
# built files (root-owned, world-readable) and listens on $PORT (>1024), so it
# needs nothing root gives it.
USER bun
EXPOSE 3000
CMD ["bun", "run", "server.ts"]
