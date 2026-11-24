FROM node:24-bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    ffmpeg \
    python3 \
 && curl -L https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/yt-dlp \
 && chmod a+rx /usr/local/bin/yt-dlp \
 && apt-get purge -y curl \
 && apt-get autoremove -y \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 1000 container
WORKDIR /home/container

ENV RSBOT_INSTALL_DIR=/home/container/rsbotbins \
    RSBOT_STATE_DIR=/home/container/rsbot-state \
    RSBOT_CONFIG=/home/container/rsbot.toml \
    NODE_ENV=production

COPY js /home/container/rsbot/js
COPY config/rsbot.example.toml /home/container/rsbot.toml

RUN mkdir -p /home/container/rsbot-state /home/container/rsbotbins \
    && chown -R container:container /home/container

USER container

# The launcher downloads static musl binaries and verifies them before spawn.
ENTRYPOINT ["node", "/home/container/rsbot/js/start.mjs"]