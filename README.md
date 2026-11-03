# OCPP Proxy - Multi-Version EV Charger Sharing

A secure, lightweight OCPP proxy server designed as a Home Assistant add-on that enables intelligent EV charger sharing. Supports both **OCPP 1.6** and **OCPP 2.0.1** protocols with automatic version detection.

The proxy sits between a single EV charger and multiple backend services (energy providers, fleet management, charging networks), allowing multiple third-party services to use spare charger capacity while keeping the homeowner in full control.

## ✨ Key Features

### 🔌 **Multi-Protocol Support**
- **OCPP 1.6** and **OCPP 2.0.1** support with automatic version detection
- WebSocket subprotocol negotiation and header-based version detection
- Unified API across both protocol versions

### 🏠 **Homeowner Control**
- User maintains full control and can override any backend at any time
- Smart arbitration with configurable rules and preferences
- Home Assistant integration for monitoring and manual overrides

### ⚡ **Intelligent Sharing**
- Single charger connection supports multiple backend subscribers
- Real-time event broadcasting to all connected services
- Automatic control arbitration with safety-first design

### 🔗 **Dual Connection Types**
- **WebSocket Backends**: Traditional services connect to the proxy
- **OCPP Service Clients**: Proxy connects outbound to OCPP services
- Both types compete for control using the same arbitration rules

### 🛡️ **Safety & Security**
- Automatic safety controls prevent conflicts and handle charger faults
- Rate limiting and provider filtering (allowlist/blocklist support)
- Charger fault detection automatically revokes backend control

### 📊 **Monitoring & Analytics**
- Session tracking with SQLite persistence
- Revenue tracking for different providers
- CSV export functionality via REST API
- Real-time status monitoring via Home Assistant

## 🚀 Installation

### Home Assistant Add-on (Recommended)

#### Via HACS (Recommended)
1. Open HACS in your Home Assistant instance
2. Go to "Integrations" → "..." → "Custom repositories"
3. Add repository URL: `https://github.com/openchargehub/ocpp-proxy`
4. Select category: "Add-on"
5. Click "Add"
6. Search for "OCPP Proxy" in HACS
7. Install the add-on
8. Configure options via the add-on UI
9. Start the add-on

#### Manual Installation
1. Copy this repository into your Home Assistant add-ons directory
2. Configure options via the add-on UI
3. Start the add-on

### Standalone (Docker Compose)
```yaml
version: '3'
services:
  ocpp_proxy:
    build: .
    environment:
      - HA_URL=http://homeassistant.local:8123
      - HA_TOKEN=YOUR_LONG_LIVED_ACCESS_TOKEN
    ports:
      - '9000:9000'
    volumes:
      - ./config:/data
```

### Development Setup
```bash
make test
make run
```

## ⚙️ Configuration

### Basic Configuration (YAML)
```yaml
# OCPP Protocol Settings
ocpp_version: "1.6"              # Default version (1.6 or 2.0.1)
auto_detect_ocpp_version: true   # Auto-detect from connection

# Sharing Control
allow_shared_charging: true
preferred_provider: "energy_provider_1"
rate_limit_seconds: 10

# Home Assistant Integration
presence_sensor: "binary_sensor.someone_home"
override_input_boolean: "input_boolean.charger_override"

# Provider Management
allowed_providers: ["provider1", "provider2"]  # Optional allowlist
blocked_providers: ["spammer"]                 # Optional blocklist

# OCPP Services (Outbound Connections)
ocpp_services:
  - id: "energy_provider"
    url: "ws://provider.com/ocpp"
    version: "1.6"
    auth_type: "basic"
    username: "user"
    password: "pass"
    enabled: true
  
  - id: "fleet_service"
    url: "wss://fleet.com/ocpp/cp001"
    version: "2.0.1"
    auth_type: "token"
    token: "bearer_token_here"
    enabled: true
```

### Version Detection
The proxy automatically detects OCPP versions through:
- WebSocket subprotocol (`ocpp1.6`, `ocpp2.0.1`)
- HTTP headers (`Sec-WebSocket-Protocol`, `X-OCPP-Version`)
- URL query parameters (`?version=2.0.1`)
- URL path patterns on the default station (`/charger/v2.0.1`)
- For a named charger, version comes from the subprotocol, `X-OCPP-Version`, or `?version=`. The id segment is not a version.

## 🔌 API Endpoints

### WebSocket Endpoints
- **`/charger`** - Default station charge point (unrepointed chargers)
- **`/charger/{id}`** - Charge point for one charger. The first connection creates that id. `v1.6`, `v2.0`, and `v2.0.1` stay on `default`.
  - Id: 1 to 48 ASCII letters, digits, `_`, or `-`. Anything else is HTTP 400.
  - Version: subprotocol, `X-OCPP-Version`, or `?version=2.0.1`
- **`/backend?id=backend_id`** - Inbound provider for the default station
- **`/charger/{id}/backend?id=backend_id`** - Inbound provider for that charger only. Does not create a charger.

### REST API
- **`GET /`** and **`GET /chargers`** - Charger list (no provider secrets)
- **`GET /chargers/{id}`** - Charger detail. Requires `ADMIN_TOKEN` via `Authorization: Bearer`, `X-Admin-Token`, or `?token=`. Missing or wrong token is 401.
- **`POST /chargers`** - Add an id before the charge point connects. Same admin token.
- **`GET /sessions`**, **`GET /sessions.csv`**, **`GET /status`**, **`POST /override`** - Public. Optional `charger_id` (query, or JSON field on override). Missing means `default`.
- Disable rejects the next charge-point and provider sockets with 403 and stops outbound dials. Delete drops the row and disconnects that station. A later charge-point connection enrolls it again.

## 🏗️ Architecture

### Core Components

```
crates/domain/        # config, version detection, control lock, session math
crates/application/   # use cases and ports
crates/ocpp-proxy/    # HTTP, OCPP-J, SQLite, Home Assistant, upstream clients
```

### Protocol Differences Handled
- **OCPP 1.6**: `RemoteStartTransaction`, `StartTransaction`/`StopTransaction`
- **OCPP 2.0.1**: `RequestStartTransaction`, `TransactionEvent` (Started/Ended)
- Automatic message format conversion and enum handling

### Control Flow
1. **Connection**: Charger connects → Version detected → Appropriate handler created
2. **Registration**: Multiple backends subscribe to events and request control
3. **Arbitration**: Smart control arbitration based on rules and preferences
4. **Safety**: Automatic fault handling and user override capabilities

## 🧪 Testing

```bash
make test
make test-domain-coverage
make test-coverage
```

Domain coverage gate: 99% lines and regions. Workspace line coverage gate: 90%. Live tests speak OCPP-J on real sockets.

## 🔧 Development Commands

```bash
make run
make test
make fmt
make clippy
make docker-build
```

## 📊 Use Cases

### Primary Scenario
Homeowner with EV charger wants to:
- Allow energy providers to use spare capacity during off-peak rates
- Let fleet services use charger for delivery vehicles during work hours
- Provide public access through charging networks when on vacation
- Maintain full control with Home Assistant automation

### Example Integrations
- **Energy Providers**: Smart charging during cheap electricity periods
- **Fleet Management**: Scheduled charging for commercial vehicles  
- **Charging Networks**: Revenue sharing through public access
- **Home Automation**: Presence-based blocking and smart overrides

## 🛡️ Safety Features

- **Fault Handling**: Charger faults immediately revoke all backend control
- **Conflict Prevention**: Only one backend can control charger simultaneously
- **User Override**: Always possible via Home Assistant interface
- **Rate Limiting**: Prevents spam requests from backends
- **Provider Filtering**: Allowlist/blocklist support for security

## 📈 Future Roadmap

### OCPP 2.1 Support (2025 H2)
- Bidirectional charging (V2G) support
- Distributed Energy Resource (DER) control
- Enhanced payment integration
- Battery swapping support

### Planned Features
- Advanced pricing APIs
- Multi-charger support
- Enhanced authentication mechanisms
- Real-time analytics dashboard

## 📄 License

Open source - check license file for details.

## 🤝 Contributing

Contributions welcome! Please:
1. Run tests: `make test`
2. Follow existing code style
3. Add tests for new features
4. Update documentation

## 🔗 Links

- [OCPP 1.6 Specification](https://openchargealliance.org/protocols/ocpp-16/)
- [OCPP 2.0.1 Specification](https://openchargealliance.org/protocols/ocpp-201/)
- [Home Assistant Add-on Development](https://developers.home-assistant.io/docs/add-ons/)