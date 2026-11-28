# Runtime never pulls or installs software. Review this source, build locally,
# then pin the resulting immutable image ID in the host OCR configuration.
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ARG DEBIAN_MIRROR=http://deb.debian.org/debian
ARG DEBIAN_SECURITY_MIRROR=http://deb.debian.org/debian-security
RUN sed -i "s|http://deb.debian.org/debian-security|${DEBIAN_SECURITY_MIRROR}|g; s|http://deb.debian.org/debian|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources
RUN apt-get update && apt-get install -y --no-install-recommends \
    python3 tesseract-ocr tesseract-ocr-eng tesseract-ocr-chi-sim \
    && rm -rf /var/lib/apt/lists/*
COPY --chmod=0555 supervisor.py /opt/taskloom/ocr-supervisor.py
RUN python3 - <<'PY'
import hashlib,json,pathlib,subprocess
root=pathlib.Path('/usr/share/tesseract-ocr/5/tessdata')
manifest={'version':1,'tesseract_package':subprocess.check_output(['/usr/bin/dpkg-query','-W','-f=${Version}','tesseract-ocr']).decode('ascii'),
          'traineddata_sha256':{name:hashlib.sha256((root/(name+'.traineddata')).read_bytes()).hexdigest() for name in ['eng','chi_sim']}}
pathlib.Path('/opt/taskloom/ocr-manifest.json').write_text(json.dumps(manifest,sort_keys=True,separators=(',',':')),encoding='ascii')
PY
LABEL org.taskloom.sandbox.contract="ocr-png-supervisor-v1"
USER 0:0
WORKDIR /tmp
ENTRYPOINT ["/usr/bin/python3", "-I", "-B", "/opt/taskloom/ocr-supervisor.py"]
