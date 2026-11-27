# Pass an explicitly resolved official Python image digest, never a mutable tag.
# docker build --build-arg BASE_IMAGE=python@sha256:... -t taskloom-sandbox:local sandbox/python
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
LABEL org.taskloom.sandbox.contract="python-supervisor-v1"
COPY --chmod=0555 supervisor.py /opt/taskloom/supervisor.py
USER 0:0
WORKDIR /tmp
ENTRYPOINT ["/usr/local/bin/python3", "-I", "-B", "/opt/taskloom/supervisor.py"]
