/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::sync::Once;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

/// Cached result of CUDA availability check
static CUDA_AVAILABLE: AtomicBool = AtomicBool::new(false);
static INIT: Once = Once::new();

/// Safely checks if CUDA is available on the system.
///
/// This function attempts to initialize CUDA and determine if it's available.
/// The result is cached after the first call, so subsequent calls are very fast.
///
/// # Returns
///
/// `true` if CUDA is available and can be initialized, `false` otherwise.
///
/// # Examples
///
/// ```
/// use monarch_rdma::is_cuda_available;
///
/// if is_cuda_available() {
///     println!("CUDA is available, can use GPU features");
/// } else {
///     println!("CUDA is not available, falling back to CPU-only mode");
/// }
/// ```
pub fn is_cuda_available() -> bool {
    INIT.call_once(|| {
        let available = check_cuda_available();
        CUDA_AVAILABLE.store(available, Ordering::SeqCst);
    });
    CUDA_AVAILABLE.load(Ordering::SeqCst)
}

/// Internal function that performs the actual CUDA availability check
fn check_cuda_available() -> bool {
    unsafe {
        // Try to initialize CUDA
        let result = cuda_sys::cuInit(0);

        if result != cuda_sys::CUresult::CUDA_SUCCESS {
            return false;
        }

        // Check if there are any CUDA devices
        let mut device_count: i32 = 0;
        let count_result = cuda_sys::cuDeviceGetCount(&mut device_count);

        if count_result != cuda_sys::CUresult::CUDA_SUCCESS || device_count <= 0 {
            return false;
        }

        // Try to get the first device to verify it's actually accessible
        let mut device: cuda_sys::CUdevice = std::mem::zeroed();
        let device_result = cuda_sys::cuDeviceGet(&mut device, 0);

        if device_result != cuda_sys::CUresult::CUDA_SUCCESS {
            return false;
        }

        true
    }
}

#[cfg(test)]
pub mod test_utils {
    use std::time::Duration;
    use std::time::Instant;

    use hyperactor::ActorRef;
    use hyperactor::Mailbox;
    use hyperactor::clock::Clock;
    use hyperactor::clock::RealClock;
    use hyperactor_mesh::Mesh;
    use hyperactor_mesh::ProcMesh;
    use hyperactor_mesh::RootActorMesh;
    use hyperactor_mesh::alloc::AllocSpec;
    use hyperactor_mesh::alloc::Allocator;
    use hyperactor_mesh::alloc::LocalAllocator;
    use ndslice::extent;

    use crate::IbverbsConfig;
    use crate::PollTarget;
    use crate::RdmaBuffer;
    use crate::cu_check;
    use crate::ibverbs_primitives::get_all_devices;
    use crate::rdma_components::RdmaQueuePair;
    use crate::rdma_manager_actor::RdmaManagerActor;
    use crate::rdma_manager_actor::RdmaManagerMessageClient;
    // Waits for the completion of an RDMA operation.

    // This function polls for the completion of an RDMA operation by repeatedly
    // sending a `PollCompletion` message to the specified actor mesh and checking
    // the returned work completion status. It continues polling until the operation
    // completes or the specified timeout is reached.

    pub async fn wait_for_completion(
        qp: &mut RdmaQueuePair,
        poll_target: PollTarget,
        timeout_secs: u64,
    ) -> Result<bool, anyhow::Error> {
        let timeout = Duration::from_secs(timeout_secs);
        let start_time = Instant::now();
        while start_time.elapsed() < timeout {
            match qp.poll_completion_target(poll_target) {
                Ok(Some(_wc)) => {
                    return Ok(true);
                }
                Ok(None) => {
                    RealClock.sleep(Duration::from_millis(1)).await;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(e));
                }
            }
        }
        Err(anyhow::Error::msg("Timeout while waiting for completion"))
    }

    /// Posts a work request to the send queue of the given RDMA queue pair.
    pub async fn send_wqe_gpu(
        qp: &mut RdmaQueuePair,
        lhandle: &RdmaBuffer,
        rhandle: &RdmaBuffer,
        op_type: u32,
    ) -> Result<(), anyhow::Error> {
        unsafe {
            let ibv_qp = qp.qp as *mut rdmaxcel_sys::ibv_qp;
            let dv_qp = qp.dv_qp as *mut rdmaxcel_sys::mlx5dv_qp;
            let params = rdmaxcel_sys::wqe_params_t {
                laddr: lhandle.addr,
                length: lhandle.size,
                lkey: lhandle.lkey,
                wr_id: u32::from_be(*(*dv_qp).dbrec.wrapping_add(1)) as u64,
                signaled: true,
                op_type,
                raddr: rhandle.addr,
                rkey: rhandle.rkey,
                qp_num: (*ibv_qp).qp_num,
                buf: (*dv_qp).sq.buf as *mut u8,
                wqe_cnt: (*dv_qp).sq.wqe_cnt,
                dbrec: (*dv_qp).dbrec,
                ..Default::default()
            };
            rdmaxcel_sys::launch_send_wqe(params);
            qp.send_wqe_idx += 1;
        }
        Ok(())
    }

    /// Posts a work request to the receive queue of the given RDMA queue pair.
    pub async fn recv_wqe_gpu(
        qp: &mut RdmaQueuePair,
        lhandle: &RdmaBuffer,
        _rhandle: &RdmaBuffer,
        op_type: u32,
    ) -> Result<(), anyhow::Error> {
        // Populate params using lhandle and rhandle
        unsafe {
            let ibv_qp = qp.qp as *mut rdmaxcel_sys::ibv_qp;
            let dv_qp = qp.dv_qp as *mut rdmaxcel_sys::mlx5dv_qp;
            let params = rdmaxcel_sys::wqe_params_t {
                laddr: lhandle.addr,
                length: lhandle.size,
                lkey: lhandle.lkey,
                wr_id: u32::from_be(*(*dv_qp).dbrec) as u64,
                op_type,
                signaled: true,
                qp_num: (*ibv_qp).qp_num,
                buf: (*dv_qp).rq.buf as *mut u8,
                wqe_cnt: (*dv_qp).rq.wqe_cnt,
                dbrec: (*dv_qp).dbrec,
                ..Default::default()
            };
            rdmaxcel_sys::launch_recv_wqe(params);
            qp.recv_wqe_idx += 1;
            qp.recv_db_idx += 1;
        }
        Ok(())
    }

    pub async fn ring_db_gpu(qp: &mut RdmaQueuePair) -> Result<(), anyhow::Error> {
        unsafe {
            let dv_qp = qp.dv_qp as *mut rdmaxcel_sys::mlx5dv_qp;
            let base_ptr = (*dv_qp).sq.buf as *mut u8;
            let wqe_cnt = (*dv_qp).sq.wqe_cnt;
            let stride = (*dv_qp).sq.stride;
            if wqe_cnt < (qp.send_wqe_idx - qp.send_db_idx) {
                return Err(anyhow::anyhow!("Overflow of WQE, possible data loss"));
            }
            while qp.send_db_idx < qp.send_wqe_idx {
                let offset = (qp.send_db_idx % wqe_cnt) * stride;
                let src_ptr = (base_ptr as *mut u8).wrapping_add(offset as usize);
                rdmaxcel_sys::launch_db_ring((*dv_qp).bf.reg, src_ptr as *mut std::ffi::c_void);
                qp.send_db_idx += 1;
            }
        }
        Ok(())
    }

    /// Wait for completion on a specific completion queue
    pub async fn wait_for_completion_gpu(
        qp: &mut RdmaQueuePair,
        poll_target: PollTarget,
        timeout_secs: u64,
    ) -> Result<bool, anyhow::Error> {
        let timeout = Duration::from_secs(timeout_secs);
        let start_time = Instant::now();

        while start_time.elapsed() < timeout {
            // Get the appropriate completion queue and index based on the poll target
            let (cq, idx, cq_type_str) = match poll_target {
                PollTarget::Send => (
                    qp.dv_send_cq as *mut rdmaxcel_sys::mlx5dv_cq,
                    qp.send_cq_idx as i32,
                    "send",
                ),
                PollTarget::Recv => (
                    qp.dv_recv_cq as *mut rdmaxcel_sys::mlx5dv_cq,
                    qp.recv_cq_idx as i32,
                    "receive",
                ),
            };

            // Poll the completion queue
            let result = unsafe { rdmaxcel_sys::launch_cqe_poll(cq as *mut std::ffi::c_void, idx) };

            match result {
                rdmaxcel_sys::CQE_POLL_TRUE => {
                    // Update the appropriate index based on the poll target
                    match poll_target {
                        PollTarget::Send => qp.send_cq_idx += 1,
                        PollTarget::Recv => qp.recv_cq_idx += 1,
                    }
                    return Ok(true);
                }
                rdmaxcel_sys::CQE_POLL_ERROR => {
                    return Err(anyhow::anyhow!("Error polling {} completion", cq_type_str));
                }
                _ => {
                    // No completion yet, sleep and try again
                    RealClock.sleep(Duration::from_millis(1)).await;
                }
            }
        }

        Err(anyhow::Error::msg("Timeout while waiting for completion"))
    }

    pub struct RdmaManagerTestEnv<'a> {
        buffer_1: Buffer,
        buffer_2: Buffer,
        pub client_1: &'a Mailbox,
        pub client_2: &'a Mailbox,
        pub actor_1: ActorRef<RdmaManagerActor>,
        pub actor_2: ActorRef<RdmaManagerActor>,
        pub rdma_handle_1: RdmaBuffer,
        pub rdma_handle_2: RdmaBuffer,
        cuda_context_1: Option<cuda_sys::CUcontext>,
        cuda_context_2: Option<cuda_sys::CUcontext>,
    }

    #[derive(Debug, Clone)]
    pub struct Buffer {
        ptr: u64,
        len: usize,
        #[allow(dead_code)]
        cpu_ref: Option<Box<[u8]>>,
    }
    impl RdmaManagerTestEnv<'_> {
        /// Sets up the RDMA test environment.
        ///
        /// This function initializes the RDMA test environment by setting up two actor meshes
        /// with their respective RDMA configurations. It also prepares two buffers for testing
        /// RDMA operations and fills the first buffer with test data.
        ///
        /// # Arguments
        ///
        /// * `buffer_size` - The size of the buffers to be used in the test.
        /// * `nics` - Tuple specifying the indices of RDMA devices to use
        /// * `accels` - Tuple specifying the indices of accelerators to use (or cpu)
        ///   both RDMAManagerActors will default to the first indexed RDMA device.
        pub async fn setup(
            buffer_size: usize,
            nics: (&str, &str),
            accels: (&str, &str),
        ) -> Result<Self, anyhow::Error> {
            let all_nic_devices = get_all_devices();
            let mut config1 = IbverbsConfig::default();
            let mut config2 = IbverbsConfig::default();

            for nic_device in all_nic_devices.iter() {
                if nic_device.name == nics.0 {
                    config1.device = nic_device.clone();
                }
                if nic_device.name == nics.1 {
                    config2.device = nic_device.clone();
                }
            }
            let accel1;
            let accel2;

            if let Some((backend, idx)) = accels.0.split_once(':') {
                assert!(backend == "cuda");
                let parsed_idx = idx
                    .parse::<usize>()
                    .expect("Device index is not a valid integer");
                accel1 = (backend.to_string(), parsed_idx);
            } else {
                assert!(accels.0 == "cpu");
                config1.use_gpu_direct = false;
                accel1 = ("cpu".to_string(), 0);
            }

            if let Some((backend, idx)) = accels.1.split_once(':') {
                assert!(backend == "cuda");
                let parsed_idx = idx
                    .parse::<usize>()
                    .expect("Device index is not a valid integer");
                accel2 = (backend.to_string(), parsed_idx);
            } else {
                assert!(accels.1 == "cpu");
                config2.use_gpu_direct = false;
                accel2 = ("cpu".to_string(), 0);
            }

            let alloc_1 = LocalAllocator
                .allocate(AllocSpec {
                    extent: extent! { proc = 1 },
                    constraints: Default::default(),
                    proc_name: None,
                })
                .await
                .unwrap();

            let proc_mesh_1 = Box::leak(Box::new(ProcMesh::allocate(alloc_1).await.unwrap()));
            let actor_mesh_1: RootActorMesh<'_, RdmaManagerActor> =
                proc_mesh_1.spawn("rdma_manager", &(config1)).await.unwrap();

            let alloc_2 = LocalAllocator
                .allocate(AllocSpec {
                    extent: extent! { proc = 1 },
                    constraints: Default::default(),
                    proc_name: None,
                })
                .await
                .unwrap();

            let proc_mesh_2 = Box::leak(Box::new(ProcMesh::allocate(alloc_2).await.unwrap()));
            let actor_mesh_2: RootActorMesh<'_, RdmaManagerActor> =
                proc_mesh_2.spawn("rdma_manager", &(config2)).await.unwrap();

            let mut buf_vec = Vec::new();
            let mut cuda_contexts = Vec::new();

            for accel in [accel1.clone(), accel2.clone()] {
                if accel.0 == "cpu" {
                    let mut buffer = vec![0u8; buffer_size].into_boxed_slice();
                    buf_vec.push(Buffer {
                        ptr: buffer.as_mut_ptr() as u64,
                        len: buffer.len(),
                        cpu_ref: Some(buffer),
                    });
                    cuda_contexts.push(None);
                    continue;
                }
                // CUDA case
                unsafe {
                    cu_check!(cuda_sys::cuInit(0));

                    let mut dptr: cuda_sys::CUdeviceptr = std::mem::zeroed();
                    let mut handle: cuda_sys::CUmemGenericAllocationHandle = std::mem::zeroed();

                    let mut device: cuda_sys::CUdevice = std::mem::zeroed();
                    cu_check!(cuda_sys::cuDeviceGet(&mut device, accel.1 as i32));

                    let mut context: cuda_sys::CUcontext = std::mem::zeroed();
                    cu_check!(cuda_sys::cuCtxCreate_v2(&mut context, 0, accel.1 as i32));
                    cu_check!(cuda_sys::cuCtxSetCurrent(context));

                    let mut granularity: usize = 0;
                    let mut prop: cuda_sys::CUmemAllocationProp = std::mem::zeroed();
                    prop.type_ = cuda_sys::CUmemAllocationType::CU_MEM_ALLOCATION_TYPE_PINNED;
                    prop.location.type_ = cuda_sys::CUmemLocationType::CU_MEM_LOCATION_TYPE_DEVICE;
                    prop.location.id = device;
                    prop.allocFlags.gpuDirectRDMACapable = 1;
                    prop.requestedHandleTypes =
                        cuda_sys::CUmemAllocationHandleType::CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR;

                    cu_check!(cuda_sys::cuMemGetAllocationGranularity(
                        &mut granularity as *mut usize,
                        &prop,
                        cuda_sys::CUmemAllocationGranularity_flags::CU_MEM_ALLOC_GRANULARITY_MINIMUM,
                    ));

                    // ensure our size is aligned
                    let /*mut*/ padded_size: usize = ((buffer_size - 1) / granularity + 1) * granularity;
                    assert!(padded_size == buffer_size);

                    cu_check!(cuda_sys::cuMemCreate(
                        &mut handle as *mut cuda_sys::CUmemGenericAllocationHandle,
                        padded_size,
                        &prop,
                        0
                    ));
                    // reserve and map the memory
                    cu_check!(cuda_sys::cuMemAddressReserve(
                        &mut dptr as *mut cuda_sys::CUdeviceptr,
                        padded_size,
                        0,
                        0,
                        0,
                    ));
                    assert!(dptr as usize % granularity == 0);
                    assert!(padded_size % granularity == 0);

                    // fails if a add cu_check macro; but passes if we don't
                    let err = cuda_sys::cuMemMap(
                        dptr as cuda_sys::CUdeviceptr,
                        padded_size,
                        0,
                        handle as cuda_sys::CUmemGenericAllocationHandle,
                        0,
                    );
                    if err != cuda_sys::CUresult::CUDA_SUCCESS {
                        panic!("failed reserving and mapping memory {:?}", err);
                    }

                    // set access
                    let mut access_desc: cuda_sys::CUmemAccessDesc = std::mem::zeroed();
                    access_desc.location.type_ =
                        cuda_sys::CUmemLocationType::CU_MEM_LOCATION_TYPE_DEVICE;
                    access_desc.location.id = device;
                    access_desc.flags =
                        cuda_sys::CUmemAccess_flags::CU_MEM_ACCESS_FLAGS_PROT_READWRITE;
                    cu_check!(cuda_sys::cuMemSetAccess(dptr, padded_size, &access_desc, 1));
                    buf_vec.push(Buffer {
                        ptr: dptr,
                        len: padded_size,
                        cpu_ref: None,
                    });
                    cuda_contexts.push(Some(context));
                }
            }

            // Fill buffer1 with test data
            if accel1.0 == "cuda" {
                let mut temp_buffer = vec![0u8; buffer_size].into_boxed_slice();
                for (i, val) in temp_buffer.iter_mut().enumerate() {
                    *val = (i % 256) as u8;
                }
                unsafe {
                    // Use the CUDA context that was created for the first buffer
                    cu_check!(cuda_sys::cuCtxSetCurrent(
                        cuda_contexts[0].expect("No CUDA context found")
                    ));

                    cu_check!(cuda_sys::cuMemcpyHtoD_v2(
                        buf_vec[0].ptr,
                        temp_buffer.as_ptr() as *const std::ffi::c_void,
                        temp_buffer.len()
                    ));
                }
            } else {
                unsafe {
                    let ptr = buf_vec[0].ptr as *mut u8; // or *const u8
                    for i in 0..buf_vec[0].len {
                        *ptr.add(i) = (i % 256) as u8;
                    }
                }
            }
            let actor_1 = actor_mesh_1.get(0).unwrap();
            let actor_2 = actor_mesh_2.get(0).unwrap();

            let rdma_handle_1 = actor_1
                .request_buffer(proc_mesh_1.client(), buf_vec[0].ptr as usize, buffer_size)
                .await?;
            let rdma_handle_2 = actor_2
                .request_buffer(proc_mesh_2.client(), buf_vec[1].ptr as usize, buffer_size)
                .await?;
            // Get keys from both actors.

            let buffer_2 = buf_vec.remove(1);
            let buffer_1 = buf_vec.remove(0);
            Ok(Self {
                buffer_1,
                buffer_2,
                client_1: proc_mesh_1.client(),
                client_2: proc_mesh_2.client(),
                actor_1,
                actor_2,
                rdma_handle_1,
                rdma_handle_2,
                cuda_context_1: cuda_contexts.get(0).cloned().flatten(),
                cuda_context_2: cuda_contexts.get(1).cloned().flatten(),
            })
        }

        pub async fn cleanup(self) -> Result<(), anyhow::Error> {
            self.actor_1
                .release_buffer(self.client_1, self.rdma_handle_1.clone())
                .await?;
            self.actor_2
                .release_buffer(self.client_2, self.rdma_handle_2.clone())
                .await?;
            if self.cuda_context_1.is_some() {
                unsafe {
                    cu_check!(cuda_sys::cuCtxSetCurrent(
                        self.cuda_context_1.expect("No CUDA context found")
                    ));
                    cu_check!(cuda_sys::cuMemUnmap(
                        self.buffer_1.ptr as cuda_sys::CUdeviceptr,
                        self.buffer_1.len
                    ));
                    cu_check!(cuda_sys::cuMemAddressFree(
                        self.buffer_1.ptr as cuda_sys::CUdeviceptr,
                        self.buffer_1.len
                    ));
                }
            }
            if self.cuda_context_2.is_some() {
                unsafe {
                    cu_check!(cuda_sys::cuCtxSetCurrent(
                        self.cuda_context_2.expect("No CUDA context found")
                    ));
                    cu_check!(cuda_sys::cuMemUnmap(
                        self.buffer_2.ptr as cuda_sys::CUdeviceptr,
                        self.buffer_2.len
                    ));
                    cu_check!(cuda_sys::cuMemAddressFree(
                        self.buffer_2.ptr as cuda_sys::CUdeviceptr,
                        self.buffer_2.len
                    ));
                }
            }
            Ok(())
        }

        pub async fn verify_buffers(&self, size: usize) -> Result<(), anyhow::Error> {
            let mut buf_vec = Vec::new();
            for (virtual_addr, cuda_context) in [
                (self.buffer_1.ptr, self.cuda_context_1),
                (self.buffer_2.ptr, self.cuda_context_2),
            ] {
                if cuda_context.is_some() {
                    let mut temp_buffer = vec![0u8; size].into_boxed_slice();
                    // SAFETY: The buffer is allocated with the correct size and the pointer is valid.
                    unsafe {
                        cu_check!(cuda_sys::cuCtxSetCurrent(
                            cuda_context.expect("No CUDA context found")
                        ));
                        cu_check!(cuda_sys::cuMemcpyDtoH_v2(
                            temp_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                            virtual_addr as cuda_sys::CUdeviceptr,
                            size
                        ));
                    }
                    buf_vec.push(Buffer {
                        ptr: temp_buffer.as_mut_ptr() as u64,
                        len: size,
                        cpu_ref: Some(temp_buffer),
                    });
                } else {
                    buf_vec.push(Buffer {
                        ptr: virtual_addr,
                        len: size,
                        cpu_ref: None,
                    });
                }
            }
            // SAFETY: The pointers are valid and the buffers have the same length.
            unsafe {
                let ptr1 = buf_vec[0].ptr as *mut u8;
                let ptr2: *mut u8 = buf_vec[1].ptr as *mut u8;
                for i in 0..buf_vec[0].len {
                    if *ptr1.add(i) != *ptr2.add(i) {
                        return Err(anyhow::anyhow!("Buffers are not equal at index {}", i));
                    }
                }
            }
            Ok(())
        }
    }
}
