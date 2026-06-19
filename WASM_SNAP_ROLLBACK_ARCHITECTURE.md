> **Early architecture concept document — does not describe the current implementation.**
> This file captures design thinking from before the codebase reached its current state.
> For the actual implementation see `src/` (entry points: `src/bin/nexus_mcp.rs`, `src/bin/nexus_agentd.rs`, `src/profile/mod.rs`).
> For current performance numbers see `BENCHMARKS.md`. For accepted design decisions see `docs/rfcs/`.

# ⚡ NEXUS: AI-Native WASM Snap-Rollback Sandbox

## Executive Summary

**Nexus** is a next-generation sandboxing infrastructure purpose-built for AI agents. Unlike traditional containers (Docker) or basic WASM runtimes, Nexus provides:

- **Microsecond snapshots** (< 5ms cold start vs Docker's 10+ seconds)
- **Hardware-enforced rollbacks** that instantly restore to pre-execution state on errors
- **Zero kernel access** unless explicitly granted via cryptographic capability tokens
- **AI-native telemetry** that feeds execution failure logs back to the model for self-correction

---

## 🎯 Core Innovation: Deterministic State Recovery

The v1 problem: AI agents crash, loop infinitely, or corrupt files → no automatic recovery → human intervention required.

The v2 solution: Every tool execution is wrapped in an atomic transaction. If the agent breaks something, the hypervisor instantly rewinds to the exact microsecond state before the error occurred. The model receives a structured failure log and autonomously finds a different solution.

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         NEXUS HYPERVISOR ENGINE                         │
│                                                                         │
│   ┌──────────────┐    ┌──────────────┐    ┌────────────────────────┐  │
│   │  SNAPSHOT    │───►│   EXECUTE    │───►│   VERIFY & VALIDATE    │  │
│   │  (pre-tool)  │    │  (WASM VM)   │    │   (health check)      │  │
│   └──────────────┘    └──────────────┘    └───────────┬────────────┘  │
│                                                        │               │
│                         ┌─────────────────────────────┘               │
│                         ▼                                               │
│   ┌──────────────────────────────────────────────────────────────┐     │
│   │                    ERROR DETECTED?                           │     │
│   │         (CPU spike / file corruption / timeout)            │     │
│   └─────────────────────────┬───────────────────────────────────┘     │
│                             │                                            │
│              ┌──────────────┴──────────────┐                           │
│              ▼                             ▼                            │
│   ┌─────────────────────┐    ┌─────────────────────┐                  │
│   │   ⚡ INSTANT ROLLBACK   │    │   ✅ CONTINUE            │                  │
│   │   Microsecond restore  │    │   Next operation       │                  │
│   │   + Error log to LLM    │    │   + Success telemetry   │                  │
│   └─────────────────────┘    └─────────────────────┘                  │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 🏗️ System Architecture

### Layer 1: Nexus Hypervisor Core

```rust
// Core hypervisor orchestrator
pub struct NexusHypervisor {
    // WASM runtime with snapshot capabilities
    engine: Arc<WasmtimeEngine>,
    
    // Snapshot store (in-memory ring buffer + optional persistence)
    snapshots: RingBuffer<Snapshot>,
    
    // Execution validator
    validator: HealthValidator,
    
    // AI telemetry sink
    telemetry: TelemetrySink,
    
    // Capability token manager
    capabilities: CapabilityManager,
}

impl NexusHypervisor {
    /// Execute a tool with mandatory pre/post snapshot
    pub async fn execute_tool(
        &self,
        tool: ToolDefinition,
        input: Value,
    ) -> Result<ToolOutput, ExecutionError> {
        // 1. Create pre-execution snapshot
        let snapshot = self.create_snapshot().await?;
        
        // 2. Execute in isolated WASM sandbox
        let result = self.execute_in_wasm(tool, input).await;
        
        // 3. Validate execution health
        let health = self.validate_health().await?;
        
        match health {
            HealthStatus::Healthy => {
                // Commit (don't rollback)
                self.commit_snapshot(snapshot)?;
                self.telemetry.record_success(&result);
                Ok(result)
            }
            HealthStatus::Corrupted => {
                // ROLLBACK - instant state recovery
                self.rollback_to(snapshot).await?;
                let error_log = self.generate_error_log(snapshot, health);
                self.telemetry.record_failure(error_log.clone());
                Err(ExecutionError::StateCorrupted(error_log))
            }
            HealthStatus::Timeout => {
                self.rollback_to(snapshot).await?;
                let error_log = self.generate_error_log(snapshot, health);
                self.telemetry.record_timeout(error_log);
                Err(ExecutionError::Timeout(error_log))
            }
        }
    }
}
```

### Layer 2: WASM Micro-Sandbox

**Why WASM over Docker?**
- Cold start: ~5ms (WASM) vs 10+ seconds (Docker)
- Security: Linear memory confinement + capability model vs shared kernel
- Portability: Runs on any OS/architecture
- Determinism: Reproducible execution across environments

```rust
pub struct WasmMicroSandbox {
    // Pre-configured with minimal WASI interface
    module: wasmtime::Module,
    linker: wasmtime::Linker,
    
    // Resource limits
    fuel: u64,           // Max instructions
    memory_limit: u64,   // Max memory in pages
    time_limit: Duration,
    
    // Ephemeral filesystem (overlay)
    fs: EphemeralFilesystem,
}

impl WasmMicroSandbox {
    pub fn new() -> Self {
        let engine = Engine::default();
        let mut linker = Linker::new(&engine);
        
        // Only grant minimal capabilities
        wasi::add_to_linker_sync(&mut linker, |ctx| ctx);
        
        // NO network access by default
        // NO filesystem write by default  
        // NO system calls beyond WASI
    }
    
    /// Execute with fuel metering (prevents infinite loops)
    pub fn execute_with_fuel(
        &self, 
        wasm_bytes: &[u8],
        fuel: u64,
    ) -> Result<ExitCode, FuelExhausted> {
        let mut store = Store::new(&self.engine, State::new());
        store.set_fuel(fuel).unwrap();
        
        // Execution will fail with FuelExhausted if infinite loop
        // This is caught and triggers rollback
    }
}
```

### Layer 3: Snapshot & Rollback Engine

```rust
#[derive(Clone)]
pub struct Snapshot {
    id: Uuid,
    timestamp: Timestamp,
    
    // Memory state (serialized WASM linear memory)
    memory_pages: Vec<u8>,
    
    // Filesystem diff (overlay changeset)
    fs_changes: FilesystemDiff,
    
    // Execution state (registers, stack, etc.)
    execution_state: ExecutionState,
    
    // Metadata
    tool_name: String,
    input_hash: Hash,
    preconditions: Vec<Capability>,
}

pub struct SnapshotManager {
    // Ring buffer for recent snapshots (configurable size)
    snapshots: RwLock<RingBuffer<Snapshot>>,
    
    // Optional persistent storage for long-term recovery
    persistent_store: Option<Box<dyn PersistentStorage>>,
    
    // Compression for memory snapshots
    compressor: ZstdCompressor,
}

impl SnapshotManager {
    /// Create a snapshot of current state
    pub async fn create_snapshot(&self) -> Result<Snapshot, SnapshotError> {
        let memory = self.capture_linear_memory().await?;
        let fs = self.capture_filesystem_diff().await?;
        let exec = self.capture_execution_state().await?;
        
        // Compress for efficiency
        let compressed = self.compressor.compress(&memory)?;
        
        Ok(Snapshot {
            id: Uuid::new_v4(),
            timestamp: Timestamp::now(),
            memory_pages: compressed,
            fs_changes: fs,
            execution_state: exec,
            // ... metadata
        })
    }
    
    /// Rollback to a specific snapshot (microsecond restore)
    pub async fn rollback_to(&self, snapshot: &Snapshot) -> Result<(), RollbackError> {
        // Decompress memory
        let memory = self.compressor.decompress(&snapshot.memory_pages)?;
        
        // Restore linear memory
        self.restore_linear_memory(&memory)?;
        
        // Revert filesystem changes
        self.revert_filesystem(&snapshot.fs_changes)?;
        
        // Restore execution state
        self.restore_execution_state(&snapshot.execution_state)?;
        
        Ok(())
    }
}
```

### Layer 4: Health Validator (AI Feedback System)

```rust
pub enum HealthStatus {
    Healthy,
    Corrupted,
    Timeout,
    ResourceExhausted,
}

pub struct HealthValidator {
    // CPU spike detection
    cpu_monitor: CpuMonitor,
    
    // Memory corruption detection
    memory_checker: MemoryChecker,
    
    // Filesystem integrity checks
    fs_integrity: FilesystemIntegrity,
    
    // Timing validation
    timeout_manager: TimeoutManager,
}

impl HealthValidator {
    pub async fn validate(&self) -> HealthStatus {
        // Check 1: CPU usage within bounds?
        if self.cpu_monitor.is_spike() {
            return HealthStatus::Corrupted;
        }
        
        // Check 2: Memory not corrupted?
        if !self.memory_checker.is_valid() {
            return HealthStatus::Corrupted;
        }
        
        // Check 3: Filesystem integrity intact?
        if !self.fs_integrity.check().await {
            return HealthStatus::Corrupted;
        }
        
        // Check 4: Execution within time limits?
        if self.timeout_manager.is_expired() {
            return HealthStatus::Timeout;
        }
        
        HealthStatus::Healthy
    }
}

/// Error log fed back to the AI model
#[derive(Serialize)]
pub struct ErrorLog {
    pub error_type: String,
    pub timestamp: DateTime,
    pub what_was_executed: String,
    pub what_went_wrong: String,
    pub system_state_at_failure: SystemState,
    pub suggested_recovery_actions: Vec<String>,
    pub previous_successful_approaches: Vec<String>,
}
```

---

## 🔐 Security Model

### Capability-Based Access Control

```rust
/// Cryptographic capability tokens - the only way to grant access
#[derive(Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub capability: Capability,
    pub granted_by: String,
    pub expires_at: Timestamp,
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    // Filesystem
    ReadFile(PathBuf),
    WriteFile(PathBuf),
    ListDirectory(PathBuf),
    
    // Network
    HttpGet(UrlPattern),
    HttpPost(UrlPattern),
    
    // System
    ExecuteBinary(PathBuf),
    MountTmpfs(PathBuf),
    
    // None (default - zero access)
    None,
}

impl CapabilityToken {
    /// Verify token is valid and not expired
    pub fn verify(&self) -> bool {
        self.signature.verify()
        && self.expires_at > Timestamp::now()
    }
    
    /// Check if token grants required capability
    pub fn allows(&self, required: &Capability) -> bool {
        match (&self.capability, required) {
            // Exact match
            (Capability::ReadFile(p1), Capability::ReadFile(p2)) => p1 == p2,
            
            // Subdirectory match (read can include subdirs)
            (Capability::ReadFile(p1), Capability::ReadFile(p2)) => p2.starts_with(p1),
            
            // Wildcard (grants all)
            (Capability::All, _) => true,
            
            _ => false,
        }
    }
}
```

### Sandboxed Execution Flow

```
1. AI Agent requests: "Read file /project/src/main.rs"
                 │
                 ▼
2. Nexus checks: Does agent have ReadFile("/project/src/main.rs")?
                 │
        ┌────────┴────────┐
        ▼                 ▼
   NO TOKEN           HAS TOKEN
        │                 │
        ▼                 ▼
   DENY ACCESS    ┌──────────────────┐
                  │                  │
                  ▼                  ▼
           ┌─────────────┐    ┌──────────────┐
           │ Verify token│    │ Execute WASM │
           │ signature   │    │ sandbox with │
           └─────────────┘    │ capability   │
                  │           └──────────────┘
                  │                  │
                  ▼                  ▼
           ┌─────────────┐    ┌──────────────┐
           │ Check       │    │ Return result│
           │ expiration  │    │ to agent     │
           └─────────────┘    └──────────────┘
```

---

## 🚀 Performance Characteristics

| Metric | Docker | Traditional VM | Nexus WASM |
|--------|--------|----------------|------------|
| Cold start | 10-30s | 1-5s | **< 5ms** |
| Memory overhead | 50-100MB | 500MB+ | **< 1MB** |
| Snapshot time | N/A | 1-5s | **< 1ms** |
| Rollback time | N/A | 500ms-2s | **< 1ms** |
| Security boundary | Kernel | Hypervisor | **Linear memory + capability** |
| Portability | Docker-specific | VM-specific | **Any platform** |

---

## 📊 AI Telemetry & Feedback Loop

Nexus doesn't just recover from failures—it feeds structured error data back to the AI model for self-correction:

```json
{
  "execution_id": "exec_12345",
  "tool": "execute_command",
  "input": "rm -rf /important",
  "error_type": "FILESYSTEM_CORRUPTION",
  "what_went_wrong": "Agent attempted to delete critical system directory",
  "system_state": {
    "files_affected": ["/important/data.db", "/important/config.yaml"],
    "reverted": true
  },
  "recovery_actions": [
    "Do not delete directories outside workspace",
    "Always confirm deletion targets with ls before rm"
  ],
  "previous_successes": [
    "Successfully deleted /tmp/cache in previous session",
    "Used safe_delete tool which checks paths"
  ],
  "suggested_approach": "Use safe_delete() instead of raw rm command"
}
```

---

## 🔧 API Design (for AI agent integration)

```typescript
interface NexusClient {
  // Initialize a new sandbox session
  createSession(config: SessionConfig): Promise<Session>;
  
  // Request a capability token
  requestCapability(capability: Capability): Promise<CapabilityToken>;
  
  // Execute a tool with automatic snapshot/rollback
  executeTool(
    tool: ToolDefinition,
    input: unknown,
    capabilities: CapabilityToken[]
  ): Promise<ToolResult>;
  
  // Get telemetry for model improvement
  getExecutionHistory(): Promise<ExecutionLog[]>;
}

interface SessionConfig {
  maxSnapshots: number;        // Ring buffer size
  memoryLimitMB: number;        // Max WASM memory
  timeLimitMs: number;         // Max execution time
  enablePersistence: boolean;  // Save snapshots to disk
}

// Example usage in AI agent
const nexus = new NexusClient('https://api.nexus-ai.dev');

await nexus.requestCapability(Capability.ReadFile('/project/src'));
await nexus.requestCapability(Capability.WriteFile('/project/src'));

const result = await nexus.executeTool(
  {
    name: 'write_file',
    wasmModule: writeFileWasm,
    inputSchema: { path: 'string', content: 'string' }
  },
  { path: '/project/src/main.rs', content: 'fn main() {}' },
  [readCapability, writeCapability]
);

// If error occurs, Nexus automatically:
// 1. Rolls back to pre-execution state
// 2. Returns structured error to agent
// 3. Agent can self-correct and retry
```

---

## 🧪 Technical Implementation Plan

### Phase 1: Core WASM Sandbox (Week 1-2)
- [ ] Set up Wasmtime engine with WASI 0.2
- [ ] Implement fuel metering for infinite loop prevention
- [ ] Basic memory snapshot/restore
- [ ] Minimal filesystem overlay (tmpfs + workspace bind)

### Phase 2: Snapshot & Rollback (Week 3-4)
- [ ] Ring buffer snapshot manager
- [ ] Zstd compression for memory snapshots
- [ ] Filesystem diff capture and revert
- [ ] Execution state serialization
- [ ] Microsecond rollback implementation

### Phase 3: Health Validation (Week 5-6)
- [ ] CPU spike detection via monitoring
- [ ] Memory corruption checksums
- [ ] Filesystem integrity verification
- [ ] Timeout management
- [ ] Structured error log generation

### Phase 4: Security Model (Week 7-8)
- [ ] Capability token issuance
- [ ] Token signature verification
- [ ] Minimal WASI interface (no network by default)
- [ ] seccomp filter integration
- [ ] cgroup resource limits

### Phase 5: AI Telemetry (Week 9-10)
- [ ] Execution history logging
- [ ] Success/failure pattern analysis
- [ ] Error log structuration for LLM feedback
- [ ] Integration with agent frameworks

### Phase 6: Production Hardening (Week 11-12)
- [ ] Performance benchmarking
- [ ] Security audit
- [ ] Multi-platform testing (Linux/macOS/Windows)
- [ ] Documentation and examples

---

## 🎯 Differentiation from Existing Solutions

| Feature | Docker | gVisor | Firecracker | **Nexus** |
|---------|--------|--------|-------------|-----------|
| Cold start | 10-30s | 100-500ms | 100-200ms | **< 5ms** |
| Snapshot/rollback | ❌ | ❌ | ✅ (slow) | **✅ (instant)** |
| AI-native telemetry | ❌ | ❌ | ❌ | **✅** |
| Fuel metering | ❌ | ❌ | ❌ | **✅** |
| Capability tokens | ❌ | Partial | Partial | **✅ (crypto)** |
| WASM-native | ❌ | ❌ | ❌ | **✅** |
| Self-healing | ❌ | ❌ | ❌ | **✅** |

---

## 🚀 Viral Potential & Market Fit

**Target Users:**
1. AI coding tools (Cursor, Copilot, Claude Code)
2. Agent frameworks (AutoGen, LangChain, CrewAI)
3. Production AI pipelines
4. Security-conscious enterprises

**One-liner pitch:** "Like a game save-state for AI agents—they can't break anything anymore."

**Demo potential:**
1. Show "AI tried to delete /system → instant rollback → AI learned and succeeded"
2. Compare cold start times: Nexus 5ms vs Docker 15s
3. Show infinite loop prevention with fuel metering

**Open source strategy:** Release core as MIT, monetize enterprise features (audit logs, persistence, multi-tenancy).

---

## 📁 Project Structure

```
nexus/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── hypervisor/
│   │   ├── mod.rs
│   │   ├── executor.rs
│   │   └── validator.rs
│   ├── sandbox/
│   │   ├── mod.rs
│   │   ├── wasm_runtime.rs
│   │   └── fuel_meter.rs
│   ├── snapshot/
│   │   ├── mod.rs
│   │   ├── manager.rs
│   │   └── compression.rs
│   ├── security/
│   │   ├── mod.rs
│   │   ├── capability.rs
│   │   └── token.rs
│   └── telemetry/
│       ├── mod.rs
│       └── error_log.rs
├── api/
│   └── nexus-api.proto
└── examples/
    └── ai_agent_demo.rs
```

---

## 🛠️ Quick Start (MVP)

```rust
use nexus::{NexusHypervisor, Capability, ToolDefinition};

#[tokio::main]
async fn main() {
    let hypervisor = NexusHypervisor::new().await;
    
    // Grant only necessary capabilities
    hypervisor.grant(Capability::ReadFile("/project".into()));
    hypervisor.grant(Capability::WriteFile("/project".into()));
    
    // Execute with automatic snapshot/rollback
    let result = hypervisor.execute_tool(
        ToolDefinition::new("read_file"),
        serde_json::json!({"path": "/project/src/main.rs"})
    ).await;
    
    match result {
        Ok(output) => println!("Success: {:?}", output),
        Err(e) => {
            // Error log already sent to AI agent
            println!("Rollback complete. Agent can self-correct.");
        }
    }
}
```

---

## 🔮 Future Extensions

1. **Distributed Snapshots**: Share snapshots across agent swarm for collective learning
2. **Predictive Rollback**: ML model predicts failure before it happens, preemptively rolls back
3. **Time-Travel Debugging**: Replay any point in execution history
4. **Collaborative Correction**: Multiple agents can "checkpoint and share" working solutions
5. **Hardware TPM Integration**: Attest snapshot integrity via TPM

---

*Design by Nexus AI Infrastructure Team | MIT License*