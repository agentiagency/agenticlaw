terraform {
  required_version = ">= 1.0"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = ">= 5.0"
    }
  }
}

provider "google" {
  project = var.project_id
  region  = regex("^(.*)-[a-z]$", var.zone)[0]
}

resource "google_compute_instance" "agenticlaw_dev" {
  name         = "agenticlaw-dev"
  machine_type = var.machine_type
  zone         = var.zone

  boot_disk {
    initialize_params {
      image = "ubuntu-os-cloud/ubuntu-2404-lts-amd64"
      size  = var.disk_size_gb
      type  = "pd-balanced"
    }
  }

  network_interface {
    network = "default"
    # No access_config — no public IP, SSH via IAP tunnel only
  }

  metadata = {
    enable-oslogin = "TRUE"
  }

  metadata_startup_script = <<-SCRIPT
    #!/bin/bash
    set -euo pipefail

    # Only run on first boot
    if [ -f /opt/.agenticlaw-provisioned ]; then
      exit 0
    fi

    apt-get update -qq
    apt-get install -y -qq build-essential pkg-config libssl-dev git

    # Install rustup for all users via shared profile
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
      RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo sh -s -- -y --default-toolchain stable

    cat > /etc/profile.d/rust.sh <<'PROFILE'
    export RUSTUP_HOME=/opt/rustup
    export CARGO_HOME=/opt/cargo
    export PATH="/opt/cargo/bin:$PATH"
    PROFILE

    touch /opt/.agenticlaw-provisioned
  SCRIPT

  tags = ["agenticlaw-dev", "iap-ssh"]

  labels = {
    purpose = "agenticlaw-dev"
    team    = "agentiagency"
  }

  scheduling {
    automatic_restart   = true
    on_host_maintenance = "MIGRATE"
    preemptible         = false
  }
}

resource "google_compute_firewall" "iap_ssh" {
  name    = "agenticlaw-dev-allow-iap-ssh"
  network = "default"

  allow {
    protocol = "tcp"
    ports    = ["22"]
  }

  # IAP's IP range
  source_ranges = ["35.235.240.0/20"]
  target_tags   = ["iap-ssh"]
}
