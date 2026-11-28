{{- define "taskloom.name" -}}taskloom{{- end -}}
{{- define "taskloom.fullname" -}}{{ printf "%s-taskloom" .Release.Name | trunc 63 | trimSuffix "-" }}{{- end -}}
{{- define "taskloom.image" -}}
{{- if .Values.image.digest -}}{{ .Values.image.repository }}@{{ .Values.image.digest }}
{{- else -}}{{ .Values.image.repository }}:{{ required "image.tag is required" .Values.image.tag }}{{- end -}}
{{- end -}}
{{- define "taskloom.labels" -}}
app.kubernetes.io/name: taskloom
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}
{{- define "taskloom.validate" -}}
{{- if gt (int .Values.queue.maxRunning) (int .Values.queue.maxOpenJobs) -}}{{ fail "queue.maxRunning must not exceed queue.maxOpenJobs" }}{{- end -}}
{{- if gt (int .Values.worker.replicas) (int .Values.queue.maxRunning) -}}{{ fail "worker.replicas must not exceed configured queue.maxRunning; update the actual SQL policy explicitly" }}{{- end -}}
{{- if and .Values.autoscaling.worker.enabled (gt (int .Values.autoscaling.worker.maxReplicas) (int .Values.queue.maxRunning)) -}}{{ fail "worker HPA maxReplicas must not exceed queue.maxRunning" }}{{- end -}}
{{- range $component := list "api" "worker" -}}
{{- $hpa := index $.Values.autoscaling $component -}}
{{- if and $hpa.enabled (gt (int $hpa.minReplicas) (int $hpa.maxReplicas)) -}}{{ fail "HPA minReplicas must not exceed maxReplicas" }}{{- end -}}
{{- if and $hpa.enabled (not (index $.Values $component).resources.requests.cpu) -}}{{ fail "CPU HPA requires explicit CPU requests" }}{{- end -}}
{{- end -}}
{{- if lt (int .Values.terminationGracePeriodSeconds) (add (int .Values.shutdownGraceSeconds) 10) -}}{{ fail "terminationGracePeriodSeconds must exceed shutdownGraceSeconds by at least 10" }}{{- end -}}
{{- if and .Values.maintenance.enabled (or .Values.autoscaling.api.enabled .Values.autoscaling.worker.enabled) -}}{{ fail "disable both HPAs before maintenance" }}{{- end -}}
{{- if and .Values.migration.enabled (not .Values.maintenance.enabled) -}}{{ fail "migration requires maintenance.enabled=true and a verified drained database" }}{{- end -}}
{{- if and .Values.migration.enabled (or (gt (int .Values.api.replicas) 0) (gt (int .Values.worker.replicas) 0)) -}}{{ fail "migration requires api.replicas=0 and worker.replicas=0, including every other host using the database" }}{{- end -}}
{{- if and .Values.migration.enabled (not (regexMatch "^[A-Za-z0-9_.-]+$" .Values.migration.operator)) -}}{{ fail "migration.operator must be a shell-safe ASCII identifier" }}{{- end -}}
{{- if and .Values.migration.enabled (not (regexMatch "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$" .Values.migration.jobId)) -}}{{ fail "migration.jobId must be a lowercase DNS label" }}{{- end -}}
{{- end -}}
