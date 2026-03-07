# Exuberant Optimizer

**Exuberant Optimizer** is a high-performance Windows system utility built in **Rust**. It provides real-time process management and memory optimization to ensure your system remains responsive during heavy workloads or gaming sessions.



## Features

### RAM Cleaner & System Timer
* **Standby List Purging:** Manually or automatically flush the Windows memory standby list.
* **Custom Thresholds:** Trigger purges based on your own "Standby List" and "Free Memory" targets.
* **System Timer Resolution:** Adjust the global Windows timer to reduce input latency and improve frame pacing.

### Process Optimization
* **SPH (Set Priority High):** Boost processes to High Priority.
* **UPC (Use Physical Cores):** Restrict processes to physical CPU cores only.
* **Real-time Stats:** Monitor CPU, RAM, and Disk I/O per process.

## Building from Source

### Prerequisites
* Windows 11
* [Rust & Cargo](https://rustup.rs/) installed
* Administrator privileges (required for system-level memory and process calls)

### Build Instructions
1. Clone the repository:
   ```bash
   git clone [https://github.com/s4106882/Exuberant-Optimizer.git](https://github.com/s4106882/Exuberant-Optimizer.git)
   cd Exuberant-Optimizer

 2. Run the Program:
    ```bash
    cargo run --release
