---
name: docker-manage
description: "Manage Docker containers, images, volumes, and Compose stacks. Requires Docker CLI access."
version: 1.0.0
metadata:
  echo:
    tags: [Docker, Container, DevOps, Deploy, Infrastructure]
    requires:
      bins: [docker]
---

# Docker Manage

Docker container and image management.

## Containers

```bash
# Status overview
docker ps -a --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}\t{{.Image}}"

# Resource usage
docker stats --no-stream --format "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.NetIO}}"

# Logs
docker logs --tail 50 <container>
docker logs --since 1h <container>

# Lifecycle
docker restart <container>
docker stop <container>
docker start <container>
```

## Images

```bash
docker images --format "table {{.Repository}}\t{{.Tag}}\t{{.Size}}\t{{.CreatedSince}}"
docker pull <image>:<tag>
docker image prune -f  # remove dangling
```

## Docker Compose

```bash
docker compose ps
docker compose up -d <service>
docker compose logs -f --tail 100 <service>
docker compose restart <service>
docker compose pull && docker compose up -d  # update all
```

## Health Checks

```bash
# Check container health
docker inspect --format='{{.State.Health.Status}}' <container>

# All unhealthy containers
docker ps --filter health=unhealthy
```

## Troubleshooting

```bash
# Shell into container
docker exec -it <container> sh

# Port conflicts
docker ps --format "{{.Ports}}" | sort

# Disk usage
docker system df

# Network inspect
docker network ls
docker network inspect <network>
```

## Safety

Always confirm before:
- `docker rm` / `docker rmi`
- `docker system prune`
- `docker compose down -v` (removes volumes!)
