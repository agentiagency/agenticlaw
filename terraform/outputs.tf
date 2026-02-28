output "instance_name" {
  value = google_compute_instance.agenticlaw_dev.name
}

output "instance_zone" {
  value = google_compute_instance.agenticlaw_dev.zone
}

output "internal_ip" {
  value = google_compute_instance.agenticlaw_dev.network_interface[0].network_ip
}

output "ssh_command" {
  value = "gcloud compute ssh ${google_compute_instance.agenticlaw_dev.name} --zone=${google_compute_instance.agenticlaw_dev.zone} --tunnel-through-iap"
}
