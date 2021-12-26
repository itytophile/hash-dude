## Build the docker images

Before building:

```sh
cargo vendor
```

## Scaling

```sh
docker compose up
python docker/monitor.py
```
