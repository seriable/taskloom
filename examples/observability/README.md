# Local OpenTelemetry receiver acceptance

All delivered artifacts here are source. No Collector binary, image or model
weights are included. `collector.yaml` uses the actual official Collector OTLP
receiver and debug exporter for **both traces and metrics**; it does not forward
data externally. Bind the published/native port to loopback when running it.

On Windows, download the pinned official asset to a fresh ignored directory:

```powershell
python examples/observability/download_collector.py target/collector-pinned
target/collector-pinned/otelcol.exe --version
target/collector-pinned/otelcol.exe --config target/collector-loopback.yaml *> target/collector.log
```

Copy `collector.yaml` to `target/collector-loopback.yaml` and change its HTTP
endpoint to `127.0.0.1:4318` (or another free loopback port) before native use.
The official **v0.140.0** Windows AMD64 release asset has SHA-256
`68239baf58f361cb03d5dd1e66fe334ee79d5ad99c3c6d6f01d90101ecd62c45`;
its actual executable self-reports **0.140.1**. Preserve that distinction.
The helper verifies the public release metadata, exact archive digest/size and
one bounded regular `otelcol.exe`, rather than extracting arbitrary archive paths.
Failed downloads remain visible. The acceptance machine used the release API
asset URL after a Docker Hub timeout and browser-download connection reset.

Alternatively use the official `otel/opentelemetry-collector:0.140.0` container,
with the config explicitly mounted and `127.0.0.1:4318:4318` published. Record
the resolved image digest before acceptance; the Windows binary evidence does
not claim that this container ran on the acceptance machine.

Add an explicit `observability.namespace` and loopback `observability.otlp`
destination to your **already configured** native generation config. The native
example requires actual loopback model API decisions at explicit zero API
prices, a zero-micro-USD generation cap and its separately bounded connector
service. It does not borrow the chat model, start a paid provider implicitly or
infer general task quality from Collector protocol acceptance.

```powershell
python examples/benchmarks/native_skills.py target/native-observability --config local-native-otlp.json --taskloom target/debug/taskloom.exe --research-package research-located
python examples/observability/accept_native.py target/native-observability --taskloom target/debug/taskloom.exe --collector-log target/collector.log
```

The latter command starts no model/service. It removes generation and connector
credential environments from CLI subprocesses and reads the saved native run.
It copies the observed before-approval snapshot into a separate inspection root,
then explicitly exports before/after and research traces to the configured local
Collector. Original stores are never rewound. Each export binds current source
revisions and destination settings. Receiver verification uses only log bytes
appended after this probe began, so old output cannot satisfy a new probe.

Pass a fresh `--name observability-2` for another create-only probe. All failed
probes remain preserved. The first development probe used the wrong workflow
filename suffix and failed before sending; later probes fixed that harness bug.
The final projection additionally includes approval, reconciliation and output
validation spans. These exports can repeat completed span IDs across snapshots;
they are separate explicit observations, not automatic network retries or extra
model calls. See `../../docs/observability.md` for API, privacy, billing, bounds,
shutdown, migration and current limitations.
