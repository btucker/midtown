> Back to [README](../README.md)

# Docker

Midtown is available as a Docker image for containerized deployments. The image includes all runtime dependencies (tmux, git, gh CLI, Claude CLI).

## Pull from Docker Hub

```bash
docker pull btucker/midtown:latest
# Or a specific version:
docker pull btucker/midtown:0.4.5
```

## Running with Docker

Mount your repository and optionally your midtown config:

```bash
# Basic usage
docker run -it --rm \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown start

# With persistent config
docker run -it --rm \
  -v ~/.midtown:/home/midtown/.midtown \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown start

# Status check
docker run --rm \
  -v ~/.midtown:/home/midtown/.midtown \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown status
```

## Building Locally

```bash
docker build -t midtown .
docker run -it --rm -v /path/to/repo:/repo -w /repo midtown start
```
