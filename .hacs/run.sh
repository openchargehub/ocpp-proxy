#!/usr/bin/with-contenv bashio

bashio::log.info "Starting OCPP Proxy..."

# Set environment variables from add-on configuration
export HA_URL="http://supervisor/core"
export HA_TOKEN="${SUPERVISOR_TOKEN}"
export ADDON_CONFIG_FILE="/data/options.json"
export LOG_DB_PATH="/data/usage_log.db"
export PORT=9000

# Log configuration
bashio::log.info "Configuration:"
bashio::log.info "- Allow shared charging: $(bashio::config 'allow_shared_charging')"
bashio::log.info "- OCPP version: $(bashio::config 'ocpp_version')"
bashio::log.info "- Auto-detect version: $(bashio::config 'auto_detect_ocpp_version')"
bashio::log.info "- Rate limit: $(bashio::config 'rate_limit_seconds')s"

# Check if Home Assistant API is available
if bashio::var.has_value "$(bashio::config 'presence_sensor')"; then
    bashio::log.info "- Presence sensor: $(bashio::config 'presence_sensor')"
fi

if bashio::var.has_value "$(bashio::config 'override_input_boolean')"; then
    bashio::log.info "- Override control: $(bashio::config 'override_input_boolean')"
fi

# Start the OCPP Proxy. The add-on shell stays bashio; the process is the Rust binary.
bashio::log.info "Starting OCPP Proxy on port ${PORT}..."
exec /usr/bin/ocpp-proxy