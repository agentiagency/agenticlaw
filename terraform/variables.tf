variable "project_id" {
  type        = string
  description = "GCP project ID"
}

variable "zone" {
  type        = string
  default     = "us-central1-a"
  description = "GCE zone for the dev instance"
}

variable "machine_type" {
  type        = string
  default     = "e2-medium"
  description = "GCE machine type"
}

variable "disk_size_gb" {
  type        = number
  default     = 50
  description = "Boot disk size in GB"
}
