# aisVerbo on a HuggingFace Docker Space (free CPU tier).
#
# The Space needs two secrets/variables:
#   ANTHROPIC_API_KEY   — consumed by the aiSlang LiteLLM stack
#   (optional) AISLANG_RELEASE_URL — a downloadable aislang binary
#
# HF runs the container as a non-root user and routes port 7860.
FROM python:3.12-slim

RUN useradd -m -u 1000 user
USER user
ENV HOME=/home/user PATH=/home/user/.local/bin:$PATH PORT=7860 HOST=0.0.0.0
WORKDIR /home/user/aisverbo

COPY --chown=user . .
RUN pip install --no-cache-dir --user -r requirements.txt

# hosted mode: per-visitor tenant partitions + the shared signing secret
# (set AISVERBO_SECRET as a Space secret; mint your admin token with the
# same secret: AISVERBO_SECRET=... python3 app/get_token.py)
ENV AISVERBO_MULTI_TENANT=1
# persistent storage (Spaces: Settings -> Persistent storage -> /data)
# ENV AISVERBO_DATA=/data

# Install the aislang CLI. Until a public release artifact exists, bake the
# binary into the image next to this Dockerfile (COPY handles it) or set
# AISLANG_RELEASE_URL at build time:
#   RUN curl -L "$AISLANG_RELEASE_URL" -o /home/user/.local/bin/aislang \
#    && chmod +x /home/user/.local/bin/aislang

# One container = the aiSlang stack + the web app.
CMD ["sh", "-c", "aislang apply aisverbo.ais && python3 app/serve.py"]
