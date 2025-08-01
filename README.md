[![Dual License](https://img.shields.io/badge/license-MIT%20and%20Apache%202.0-blue)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/orderbook-rs.svg)](https://crates.io/crates/orderbook-rs)
[![Downloads](https://img.shields.io/crates/d/orderbook-rs.svg)](https://crates.io/crates/orderbook-rs)
[![Stars](https://img.shields.io/github/stars/joaquinbejar/OrderBook-rs.svg)](https://github.com/joaquinbejar/OrderBook-rs/stargazers)
[![Issues](https://img.shields.io/github/issues/joaquinbejar/OrderBook-rs.svg)](https://github.com/joaquinbejar/OrderBook-rs/issues)
[![PRs](https://img.shields.io/github/issues-pr/joaquinbejar/OrderBook-rs.svg)](https://github.com/joaquinbejar/OrderBook-rs/pulls)

[![Build Status](https://img.shields.io/github/workflow/status/joaquinbejar/OrderBook-rs/CI)](https://github.com/joaquinbejar/OrderBook-rs/actions)
[![Coverage](https://img.shields.io/codecov/c/github/joaquinbejar/OrderBook-rs)](https://codecov.io/gh/joaquinbejar/OrderBook-rs)
[![Dependencies](https://img.shields.io/librariesio/github/joaquinbejar/OrderBook-rs)](https://libraries.io/github/joaquinbejar/OrderBook-rs)
[![Documentation](https://img.shields.io/badge/docs-latest-blue.svg)](https://docs.rs/orderbook-rs)



## High-Performance Lock-Free Order Book Engine

A high-performance, thread-safe limit order book implementation written in Rust. This project provides a comprehensive order matching engine designed for low-latency trading systems, with a focus on concurrent access patterns and lock-free data structures.

### Key Features

- **Lock-Free Architecture**: Built using atomics and lock-free data structures to minimize contention and maximize throughput in high-frequency trading scenarios.

- **Multiple Order Types**: Support for various order types including standard limit orders, iceberg orders, post-only, fill-or-kill, immediate-or-cancel, good-till-date, trailing stop, pegged, market-to-limit, and reserve orders with custom replenishment logic.

- **Thread-Safe Price Levels**: Each price level can be independently and concurrently modified by multiple threads without blocking.

- **Advanced Order Matching**: Efficient matching algorithm for both market and limit orders, correctly handling complex order types and partial fills.

- **Performance Metrics**: Built-in statistics tracking for benchmarking and monitoring system performance.

- **Memory Efficient**: Designed to scale to millions of orders with minimal memory overhead.

### Design Goals

This order book engine is built with the following design principles:

1. **Correctness**: Ensure that all operations maintain the integrity of the order book, even under high concurrency.
2. **Performance**: Optimize for low latency and high throughput in both write-heavy and read-heavy workloads.
3. **Scalability**: Support for millions of orders and thousands of price levels without degradation.
4. **Flexibility**: Easily extendable to support additional order types and matching algorithms.

### Use Cases

- **Trading Systems**: Core component for building trading systems and exchanges
- **Market Simulation**: Tool for back-testing trading strategies with realistic market dynamics
- **Research**: Platform for studying market microstructure and order flow
- **Educational**: Reference implementation for understanding modern exchange architecture

### What's New in Version 0.2.0

This version introduces significant performance optimizations and architectural improvements:

- **Performance Boost**: Reintroduced `PriceLevelCache` for faster best bid/ask lookups and a `MatchingPool` to reduce memory allocations in the matching engine, leading to lower latency.
- **Cleaner Architecture**: Refactored modification and matching logic for better separation of concerns and maintainability.
- **Enhanced Concurrency**: Improved thread-safe operations, ensuring robustness under heavy load.
- **Improved Documentation**: All code comments have been translated to English, and crate-level documentation has been expanded for clarity.

### Status
This project is currently in active development and is not yet suitable for production use.

## Performance Analysis of the OrderBook System

This analyzes the performance of the OrderBook system based on tests conducted on an Apple M4 Max processor. The data comes from two types of tests: a High-Frequency Trading (HFT) simulation and contention pattern tests.

### 1. High-Frequency Trading (HFT) Simulation

#### Test Configuration
- **Symbol:** BTC/USD
- **Duration:** 5000 ms (5 seconds)
- **Threads:** 30 threads total
  - 10 maker threads (order creators)
  - 10 taker threads (order executors)
  - 10 canceller threads (order cancellers)
- **Initial orders:** 1020 pre-loaded orders

#### Performance Results

| Metric | Total Operations | Operations/Second |
|---------|---------------------|---------------------|
| Orders Added | 559,266 | 111,844.44 |
| Orders Matched | 330,638 | 66,122.42 |
| Orders Cancelled | 4,106,360 | 821,207.71 |
| **Total Operations** | **4,996,264** | **999,174.58** |

#### Initial vs. Final OrderBook State

| Metric | Initial State | Final State |
|---------|----------------|--------------|
| Best Bid | 9,900 | 9,880 |
| Best Ask | 10,000 | 10,050 |
| Spread | 100 | 170 |
| Mid Price | 9,950.00 | 9,965.00 |
| Total Orders | 1,020 | 138,295 |
| Bid Price Levels | 21 | 11 |
| Ask Price Levels | 21 | 11 |
| Total Bid Quantity | 7,750 | 1,037,923 |
| Total Ask Quantity | 7,750 | 1,488,201 |

### 2. Contention Pattern Tests

#### Configuration
- **Threads:** 12
- **Duration per test:** 3000 ms (3 seconds)

#### Read/Write Ratio Test

| Read % | Operations/Second |
|------------|---------------------|
| 0%         | 430,081.91          |
| 25%        | 17,031.12           |
| 50%        | 15,965.15           |
| 75%        | 20,590.32           |
| 95%        | 42,451.24           |

#### Hot Spot Contention Test

| % Operations on Hot Spot | Operations/Second   |
|--------------------------|---------------------|
| 0%                       | 2,742,810.37        |
| 25%                      | 3,414,940.27        |
| 50%                      | 4,542,931.02        |
| 75%                      | 8,834,677.82        |
| 100%                     | 19,403,341.34       |

#### Performance Improvements and Deadlock Resolution

The significant performance gains, especially in the "Hot Spot Contention Test," and the resolution of the previous deadlocks are a direct result of refactoring the internal concurrency model of the `PriceLevel`.

- **Previous Bottleneck:** The original implementation relied on a `crossbeam::queue::SegQueue` for storing orders. While the queue itself is lock-free, operations like finding or removing a specific order required draining the entire queue into a temporary list, performing the action, and then pushing all elements back. This process was inefficient and created a major point of contention, leading to deadlocks under heavy multi-threaded load.

- **New Implementation:** The `OrderQueue` was re-designed to use a combination of:
  1. A `dashmap::DashMap` for storing orders, allowing for highly concurrent, O(1) average-case time complexity for insertions, lookups, and removals by `OrderId`.
  2. A `crossbeam::queue::SegQueue` that now only stores `OrderId`s to maintain the crucial First-In-First-Out (FIFO) order for matching.

This hybrid approach eliminates the previous bottleneck, allowing threads to operate on the order collection with minimal contention, which is reflected in the massive throughput increase in the hot spot tests.

### 3. Analysis and Conclusions

#### Overall Performance
The system demonstrates an impressive capability to handle nearly **1 million operations per second** in the high-frequency trading simulation, distributed across order creations, matches, and cancellations.

#### Read/Write Behavior
- **Notable observation:** Performance is highest with 0% and 95% read operations, showing a U-shaped curve.
- Pure write operations (0% reads) are extremely fast (716,117 ops/s).
- Performance significantly improves when most operations are reads (95% reads = 73,484 ops/s).
- Performance is lowest in the middle range (50% reads = 29,525 ops/s), indicating that the mix of reads and writes creates more contention.

#### Hot Spot Contention
- Surprisingly, performance **increases** as more operations concentrate on a hot spot, reaching its maximum with 100% concentration (28,327,212 ops/s).
- This counter-intuitive behavior might indicate:
  1. Very efficient cache effects when operations are concentrated in one memory area
  2. Internal optimizations to handle high-contention cases
  3. Benefits of the system's lock-free architecture

#### OrderBook State Behavior
- During the HFT simulation, the order book handled a massive increase in order volume (from 1,020 to 87,155).
- The spread increased from 100 to 270, reflecting realistic market behavior under pressure.
- The concentration of orders changed significantly, with fewer price levels but higher volume at each level.

### 4. Practical Implications

- The system is suitable for high-frequency trading environments with the capacity to process nearly 1 million operations per second.
- The lock-free architecture proves to be extremely effective at handling contention, especially at hot spots.
- Optimal performance is achieved when the workload is dominated by a single type of operation (mostly reads or mostly writes).
- For real-world use cases, it would be advisable to design the workload distribution to avoid intermediate read/write ratios (25-75%), which show the lowest performance.

This analysis confirms that the system design is highly scalable and appropriate for demanding financial applications requiring high-speed processing with data consistency.


## 🛠 Makefile Commands

This project includes a `Makefile` with common tasks to simplify development. Here's a list of useful commands:

### 🔧 Build & Run

```sh
make build         # Compile the project
make release       # Build in release mode
make run           # Run the main binary
```

### 🧪 Test & Quality

```sh
make test          # Run all tests
make fmt           # Format code
make fmt-check     # Check formatting without applying
make lint          # Run clippy with warnings as errors
make lint-fix      # Auto-fix lint issues
make fix           # Auto-fix Rust compiler suggestions
make check         # Run fmt-check + lint + test
```

### 📦 Packaging & Docs

```sh
make doc           # Check for missing docs via clippy
make doc-open      # Build and open Rust documentation
make create-doc    # Generate internal docs
make readme        # Regenerate README using cargo-readme
make publish       # Prepare and publish crate to crates.io
```

### 📈 Coverage & Benchmarks

```sh
make coverage            # Generate code coverage report (XML)
make coverage-html       # Generate HTML coverage report
make open-coverage       # Open HTML report
make bench               # Run benchmarks using Criterion
make bench-show          # Open benchmark report
make bench-save          # Save benchmark history snapshot
make bench-compare       # Compare benchmark runs
make bench-json          # Output benchmarks in JSON
make bench-clean         # Remove benchmark data
```

### 🧪 Git & Workflow Helpers

```sh
make git-log             # Show commits on current branch vs main
make check-spanish       # Check for Spanish words in code
make zip                 # Create zip without target/ and temp files
make tree                # Visualize project tree (excludes common clutter)
```

### 🤖 GitHub Actions (via act)

```sh
make workflow-build      # Simulate build workflow
make workflow-lint       # Simulate lint workflow
make workflow-test       # Simulate test workflow
make workflow-coverage   # Simulate coverage workflow
make workflow            # Run all workflows
```

ℹ️ Requires act for local workflow simulation and cargo-tarpaulin for coverage.

## Contribution and Contact

We welcome contributions to this project! If you would like to contribute, please follow these steps:

1. Fork the repository.
2. Create a new branch for your feature or bug fix.
3. Make your changes and ensure that the project still builds and all tests pass.
4. Commit your changes and push your branch to your forked repository.
5. Submit a pull request to the main repository.

If you have any questions, issues, or would like to provide feedback, please feel free to contact the project
maintainer:

**Joaquín Béjar García**

- Email: jb@taunais.com
- GitHub: [joaquinbejar](https://github.com/joaquinbejar)

We appreciate your interest and look forward to your contributions!

License: MIT
