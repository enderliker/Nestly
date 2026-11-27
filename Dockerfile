FROM node:24-bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    ffmpeg \
    python3 \
 && arch="$(dpkg --print-architecture)" \
 && if [ "$arch" = "amd64" ]; then \
      curl -fsSL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux.zip -o /tmp/yt-dlp.zip \
      && python3 -m zipfile -e /tmp/yt-dlp.zip /usr/local/lib/yt-dlp \
      && rm -f /tmp/yt-dlp.zip \
      && chmod a+rx /usr/local/lib/yt-dlp/yt-dlp_linux \
      && printf '#!/bin/sh\nexec /usr/local/lib/yt-dlp/yt-dlp_linux "$@"\n' > /usr/local/bin/yt-dlp; \
    else \
      curl -fsSL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/yt-dlp; \
    fi \
 && chmod a+rx /usr/local/bin/yt-dlp \
 && apt-get purge -y curl \
 && apt-get autoremove -y \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 1000 container
WORKDIR /home/container

ENV NESTLY_INSTALL_DIR=/home/container/nestlybins \
    NESTLY_STATE_DIR=/home/container/nestly-state \
    NESTLY_CONFIG=/home/container/nestly.toml \
    RSBOT_INSTALL_DIR=/home/container/nestlybins \
    RSBOT_STATE_DIR=/home/container/nestly-state \
    RSBOT_CONFIG=/home/container/nestly.toml \
    NODE_ENV=production

COPY js /home/container/nestly/js
COPY config/nestly.example.toml /home/container/nestly.toml

RUN mkdir -p /home/container/nestly-state /home/container/nestlybins \
    && chown -R container:container /home/container

USER container

# The launcher downloads static musl binaries and verifies them before spawn.
ENTRYPOINT ["node", "/home/container/nestly/js/start.mjs"]