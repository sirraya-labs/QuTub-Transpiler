#!/usr/bin/env bash

set -e

echo "Creating Sirraya QuTub documentation structure..."

# -----------------------------------------------------------------------------
# Root Pages
# -----------------------------------------------------------------------------

touch 01-introduction.md
touch 02-getting-started.md
touch 03-installation.md
touch 04-first-program.md
touch 05-roadmap.md
touch 06-faq.md
touch 07-changelog.md

# -----------------------------------------------------------------------------
# Architecture
# -----------------------------------------------------------------------------

mkdir -p architecture

touch architecture/01-overview.md
touch architecture/02-compiler-pipeline.md
touch architecture/03-intermediate-representation.md
touch architecture/04-source-optimization.md
touch architecture/05-routing.md
touch architecture/06-native-decomposition.md
touch architecture/07-backend-lowering.md
touch architecture/08-validation.md
touch architecture/09-pulse-compilation.md

# -----------------------------------------------------------------------------
# User Guide
# -----------------------------------------------------------------------------

mkdir -p guides

touch guides/01-parsing-qasm.md
touch guides/02-circuit-optimization.md
touch guides/03-routing.md
touch guides/04-hardware-backends.md
touch guides/05-exporting-circuits.md
touch guides/06-visualization.md

# -----------------------------------------------------------------------------
# Examples
# -----------------------------------------------------------------------------

mkdir -p examples

touch examples/01-bell-state.md
touch examples/02-ghz-state.md
touch examples/03-grover-search.md
touch examples/04-quantum-fourier-transform.md
touch examples/05-teleportation.md
touch examples/06-routing-example.md
touch examples/07-noise-simulation.md

# -----------------------------------------------------------------------------
# API
# -----------------------------------------------------------------------------

mkdir -p api

touch api/01-api-overview.md
touch api/02-circuit.md
touch api/03-parser.md
touch api/04-routing.md
touch api/05-backends.md
touch api/06-simulator.md

# -----------------------------------------------------------------------------
# Developer Guide
# -----------------------------------------------------------------------------

mkdir -p developer

touch developer/01-repository-structure.md
touch developer/02-adding-gates.md
touch developer/03-writing-compiler-passes.md
touch developer/04-adding-hardware-backends.md
touch developer/05-testing.md
touch developer/06-benchmarks.md
touch developer/07-contributing.md

# -----------------------------------------------------------------------------
# Assets
# -----------------------------------------------------------------------------

mkdir -p assets/images
mkdir -p assets/diagrams

# -----------------------------------------------------------------------------
# README placeholders
# -----------------------------------------------------------------------------

touch architecture/README.md
touch guides/README.md
touch examples/README.md
touch api/README.md
touch developer/README.md

echo ""
echo "Documentation structure created successfully!"
echo ""
echo "docs/"
tree .