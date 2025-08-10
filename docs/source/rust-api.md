# Rust API Documentation

This page provides access to the Rust API documentation for Monarch.

The Rust API documentation is automatically generated from the source code using Rustdoc.

## Accessing the Rust API Documentation

<div id="rust-api-links">
    <p>You can access the full Rust API documentation here:</p>
    <a id="main-api-link" href="rust-api/index.html" class="btn btn-primary" style="display: none;">View Complete Rust API Documentation</a>
    <p id="loading-message">Loading API documentation...</p>
</div>

## Individual Crate Documentation

The Monarch project consists of several Rust crates, each with specialized functionality:

### Core Framework
- <a id="link-hyperactor" href="rust-api/hyperactor/index.html" style="display: none;">**hyperactor**</a><span id="desc-hyperactor" style="display: none;"> - Core actor framework for distributed computing</span>
- <a id="link-hyperactor_macros" href="rust-api/hyperactor_macros/index.html" style="display: none;">**hyperactor_macros**</a><span id="desc-hyperactor_macros" style="display: none;"> - Procedural macros for the hyperactor framework</span>
- <a id="link-hyperactor_multiprocess" href="rust-api/hyperactor_multiprocess/index.html" style="display: none;">**hyperactor_multiprocess**</a><span id="desc-hyperactor_multiprocess" style="display: none;"> - Multi-process support for hyperactor</span>
- <a id="link-hyperactor_mesh" href="rust-api/hyperactor_mesh/index.html" style="display: none;">**hyperactor_mesh**</a><span id="desc-hyperactor_mesh" style="display: none;"> - Mesh networking for hyperactor clusters</span>
- <a id="link-hyperactor_mesh_macros" href="rust-api/hyperactor_mesh_macros/index.html" style="display: none;">**hyperactor_mesh_macros**</a><span id="desc-hyperactor_mesh_macros" style="display: none;"> - Macros for hyperactor mesh functionality</span>

### CUDA and GPU Computing
- <a id="link-cuda-sys" href="rust-api/cuda_sys/index.html" style="display: none;">**cuda-sys**</a><span id="desc-cuda-sys" style="display: none;"> - Low-level CUDA FFI bindings</span>
- <a id="link-nccl-sys" href="rust-api/nccl_sys/index.html" style="display: none;">**nccl-sys**</a><span id="desc-nccl-sys" style="display: none;"> - NCCL (NVIDIA Collective Communications Library) bindings</span>
- <a id="link-torch-sys" href="rust-api/torch_sys/index.html" style="display: none;">**torch-sys**</a><span id="desc-torch-sys" style="display: none;"> - PyTorch C++ API bindings for Rust</span>
- <a id="link-monarch_tensor_worker" href="rust-api/monarch_tensor_worker/index.html" style="display: none;">**monarch_tensor_worker**</a><span id="desc-monarch_tensor_worker" style="display: none;"> - High-performance tensor processing worker</span>

### System and Utilities
- <a id="link-controller" href="rust-api/controller/index.html" style="display: none;">**controller**</a><span id="desc-controller" style="display: none;"> - System controller and orchestration</span>
- <a id="link-hyper" href="rust-api/hyper/index.html" style="display: none;">**hyper**</a><span id="desc-hyper" style="display: none;"> - HTTP utilities and web service support</span>
- <a id="link-ndslice" href="rust-api/ndslice/index.html" style="display: none;">**ndslice**</a><span id="desc-ndslice" style="display: none;"> - N-dimensional array slicing and manipulation</span>

<div id="no-api-message" style="display: none; color: #666; font-style: italic;">
    <p>Rust API documentation is being generated. Please check back later or refer to the source code.</p>
</div>

<script>
document.addEventListener('DOMContentLoaded', function() {
    // List of all crate names that should be documented
    const crates = [
        'hyperactor', 'hyperactor_macros', 'hyperactor_multiprocess',
        'hyperactor_mesh', 'hyperactor_mesh_macros', 'cuda_sys',
        'nccl_sys', 'torch_sys', 'monarch_tensor_worker',
        'controller', 'hyper', 'ndslice'
    ];

    let availableCrates = 0;
    let totalChecked = 0;

    // Function to check if a crate's documentation exists
    function checkCrate(crateName) {
        return fetch(`rust-api/${crateName}/index.html`, { method: 'HEAD' })
            .then(response => {
                totalChecked++;
                if (response.ok) {
                    availableCrates++;
                    // Show the link and description for this crate
                    const link = document.getElementById(`link-${crateName.replace('_', '_')}`);
                    const desc = document.getElementById(`desc-${crateName.replace('_', '_')}`);
                    if (link) link.style.display = 'inline';
                    if (desc) desc.style.display = 'inline';
                    return true;
                }
                return false;
            })
            .catch(() => {
                totalChecked++;
                return false;
            });
    }

    // Check for main index.html
    fetch('rust-api/index.html', { method: 'HEAD' })
        .then(response => {
            if (response.ok) {
                document.getElementById('main-api-link').style.display = 'inline-block';
            }
        })
        .catch(() => {
            // Main index not found, continue with individual checks
        });

    // Check all crates
    Promise.all(crates.map(checkCrate))
        .then(() => {
            document.getElementById('loading-message').style.display = 'none';

            if (availableCrates === 0) {
                document.getElementById('no-api-message').style.display = 'block';
            }
        });
});
</script>

## Architecture Overview

The Rust implementation provides a comprehensive framework for distributed computing with GPU acceleration:

- **Actor Model**: Built on the hyperactor framework for concurrent, distributed processing
- **GPU Integration**: Native CUDA support for high-performance computing workloads
- **Mesh Networking**: Efficient communication between distributed nodes
- **Tensor Operations**: Optimized tensor processing with PyTorch integration
- **Multi-dimensional Arrays**: Advanced slicing and manipulation of n-dimensional data

For complete technical details, API references, and usage examples, explore the individual crate documentation above.
