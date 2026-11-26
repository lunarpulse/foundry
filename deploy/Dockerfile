# Offline packaging context is deploy/. BASE_REPO and its manifest digest must
# identify approved, preloaded Python, Git and /opt/foundry/die.
ARG BASE_REPO
ARG BASE_SHA256
FROM ${BASE_REPO}@sha256:${BASE_SHA256}
COPY --chmod=0555 worker.py /opt/foundry/worker.py
COPY --chmod=0555 model-client.py /opt/foundry/model-client
USER 65534:65534
WORKDIR /work
ENTRYPOINT ["/usr/bin/python3","-I","/opt/foundry/worker.py"]
