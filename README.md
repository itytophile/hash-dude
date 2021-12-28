## Build the docker images

Before building:

```sh
cargo vendor
```

## Monitoring

After creating the swarm:

```sh
docker stack deploy --compose-file docker-compose.yml stackhash
python docker/monitor.py
```

To delete the stack:

```sh
docker stack rm stackhash
```
