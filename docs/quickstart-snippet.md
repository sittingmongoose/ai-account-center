<!-- quickstart-snippet-start -->
## Quick Start (Docker)

From a checkout of [AI Account Center](https://github.com/sittingmongoose/ai-account-center):

```bash
docker compose -f docker/compose.yaml up -d --build
```

Open [the dashboard](http://localhost:3000). This builds the local source image;
it does not download the upstream CCS package or start a CLIProxy service.
Existing host credentials and usage-collector connections require explicit
configuration; see the Docker deployment guide in this checkout.
<!-- quickstart-snippet-end -->
