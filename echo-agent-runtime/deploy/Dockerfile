FROM node:24-bookworm-slim AS dashboard
WORKDIR /src/web
RUN npm install --global pnpm@10.34.5
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

FROM python:3.11-slim-bookworm AS builder
WORKDIR /src
COPY . .
COPY --from=dashboard /src/web/dist ./web/dist
RUN python -m pip install --no-cache-dir build && python -m build --wheel

FROM python:3.11-slim-bookworm
RUN apt-get update && apt-get install --no-install-recommends -y ca-certificates libgomp1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/dist /tmp/wheels
RUN for wheel in /tmp/wheels/*.whl; do python -m pip install --no-cache-dir "${wheel}[openai]"; done \
    && rm -rf /tmp/wheels \
    && useradd --create-home --uid 10001 echoagent \
    && mkdir -p /data/workspace && chown -R echoagent:echoagent /data
USER echoagent
ENV PYTHONUNBUFFERED=1
WORKDIR /data
EXPOSE 58123
ENTRYPOINT ["echo-agent", "gateway", "--config", "/data/echo-agent.yaml", "--workspace", "/data/workspace"]
