#!/bin/zsh

# swarm init si pas swarm init
if [ $(docker info --format '{{.Swarm.ControlAvailable}}') = "false" ]; then
  docker swarm init
fi

# Ajoute un registry pour pouvoir push en local des images docker
if [ ! -n "$(docker service ls | grep registry)" ]; then
    docker service create --name registry --publish published=5000,target=5000 registry:2
fi

# Si les services existent déjà, on les supprime.
if [ -n "$(docker stack ls | grep stackdemo)" ]; then
  docker stack rm stackdemo
fi

docker-compose build
docker-compose push
sleep 5
docker stack deploy --resolve-image=never --compose-file docker-compose.yml stackdemo
