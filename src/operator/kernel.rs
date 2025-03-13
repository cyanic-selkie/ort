use alloc::{boxed::Box, ffi::CString, string::String, vec, vec::Vec};
use core::{
	ffi::{c_char, c_void},
	mem::size_of,
	ptr::{self, NonNull},
	slice
};

use crate::{
	AsPointer,
	error::{Error, Result},
	memory::{Allocator, MemoryInfo, MemoryType},
	ortsys,
	session::{Input, Output},
	tensor::Shape,
	util::with_cstr,
	value::{DowncastableTarget, DynValue, Value, ValueRef, ValueRefMut, ValueType}
};

pub trait Kernel {
	fn compute(&mut self, ctx: &KernelContext) -> crate::Result<()>;
}

impl<F> Kernel for F
where
	F: FnMut(&KernelContext) -> crate::Result<()>
{
	fn compute(&mut self, ctx: &KernelContext) -> crate::Result<()> {
		self(ctx)
	}
}

pub struct KernelAttributes {
	ptr: NonNull<ort_sys::OrtKernelInfo>,
	should_release: bool
}

impl KernelAttributes {
	pub(crate) fn from_ptr(ptr: NonNull<ort_sys::OrtKernelInfo>, should_release: bool) -> Self {
		Self { ptr, should_release }
	}

	pub fn get<'s, T: GetKernelAttribute<'s>>(&'s self, name: impl AsRef<str>) -> Option<T> {
		with_cstr(name.as_ref().as_bytes(), &|name| unsafe { T::get_from(self.ptr.as_ptr(), name.as_ptr()) }).ok()
	}

	pub fn inputs(&self) -> Result<Vec<Input>> {
		let mut num_inputs = 0;
		ortsys![unsafe KernelInfo_GetInputCount(self.ptr.as_ptr(), &mut num_inputs)?];

		let mut inputs = Vec::with_capacity(num_inputs);
		for idx in 0..num_inputs {
			let mut name_len = 0;
			ortsys![unsafe KernelInfo_GetInputName(self.ptr.as_ptr(), idx, ptr::null_mut(), &mut name_len)?];
			let mut name = vec![0u8; name_len];
			ortsys![unsafe KernelInfo_GetInputName(self.ptr.as_ptr(), idx, name.as_mut_ptr().cast::<c_char>(), &mut name_len)?];
			let name = CString::from_vec_with_nul(name)
				.map_err(Error::wrap)?
				.into_string()
				.map_err(Error::wrap)?;
			let mut type_info = ptr::null_mut();
			ortsys![unsafe KernelInfo_GetInputTypeInfo(self.ptr.as_ptr(), idx, &mut type_info)?; nonNull(type_info)];
			let input_type = ValueType::from_type_info(type_info);
			inputs.push(Input { name, input_type })
		}
		Ok(inputs)
	}

	pub fn outputs(&self) -> Result<Vec<Output>> {
		let mut num_outputs = 0;
		ortsys![unsafe KernelInfo_GetOutputCount(self.ptr.as_ptr(), &mut num_outputs)?];

		let mut outputs = Vec::with_capacity(num_outputs);
		for idx in 0..num_outputs {
			let mut name_len = 0;
			ortsys![unsafe KernelInfo_GetOutputName(self.ptr.as_ptr(), idx, ptr::null_mut(), &mut name_len)?];
			let mut name = vec![0u8; name_len];
			ortsys![unsafe KernelInfo_GetOutputName(self.ptr.as_ptr(), idx, name.as_mut_ptr().cast::<c_char>(), &mut name_len)?];
			let name = CString::from_vec_with_nul(name)
				.map_err(Error::wrap)?
				.into_string()
				.map_err(Error::wrap)?;
			let mut type_info = ptr::null_mut();
			ortsys![unsafe KernelInfo_GetOutputTypeInfo(self.ptr.as_ptr(), idx, &mut type_info)?; nonNull(type_info)];
			let output_type = ValueType::from_type_info(type_info);
			outputs.push(Output { name, output_type })
		}
		Ok(outputs)
	}

	pub fn constant_input<T: DowncastableTarget>(&self, idx: usize) -> Result<ValueRef<'_, T>> {
		let mut value_ptr: *const ort_sys::OrtValue = ptr::null();
		let mut is_constant = 0;
		ortsys![unsafe KernelInfoGetConstantInput_tensor(self.ptr.as_ptr(), idx, &mut is_constant, &mut value_ptr)?];
		if is_constant == 0 || value_ptr.is_null() {
			return Err(Error::new("input index out of bounds or input is not constant"));
		}

		unsafe { ValueRef::new(DynValue::from_ptr_nodrop(NonNull::new_unchecked(value_ptr.cast_mut()), None)) }.downcast()
	}

	pub fn node_name(&self) -> Result<String> {
		let mut name_len = 0;
		ortsys![unsafe KernelInfo_GetNodeName(self.ptr.as_ptr(), ptr::null_mut(), &mut name_len)?];
		let mut name = vec![0u8; name_len];
		ortsys![unsafe KernelInfo_GetNodeName(self.ptr.as_ptr(), name.as_mut_ptr().cast::<c_char>(), &mut name_len)?];
		CString::from_vec_with_nul(name).map_err(Error::wrap)?.into_string().map_err(Error::wrap)
	}

	pub fn allocator(&self, mem_type: MemoryType) -> Result<Allocator> {
		let mut ptr: *mut ort_sys::OrtAllocator = ptr::null_mut();
		ortsys![unsafe KernelInfoGetAllocator(self.ptr.as_ptr(), mem_type.into(), &mut ptr)?];
		Ok(unsafe { Allocator::from_raw_unchecked(ptr) })
	}
}

impl Clone for KernelAttributes {
	fn clone(&self) -> Self {
		let mut out = ptr::null_mut();
		ortsys![unsafe CopyKernelInfo(self.ptr.as_ptr(), &mut out).expect("failed to clone KernelAttributes")];
		Self {
			ptr: NonNull::new(out).expect("failed to clone KernelAttributes"),
			should_release: true
		}
	}
}

impl AsPointer for KernelAttributes {
	type Sys = ort_sys::OrtKernelInfo;

	fn ptr(&self) -> *const Self::Sys {
		self.ptr.as_ptr()
	}
}

impl Drop for KernelAttributes {
	fn drop(&mut self) {
		if self.should_release {
			ortsys![unsafe ReleaseKernelInfo(self.ptr.as_ptr())];
		}
	}
}

pub trait GetKernelAttribute<'s> {
	fn attr_type() -> Option<ort_sys::OrtOpAttrType> {
		None
	}

	unsafe fn from_read_op(attr: *const ort_sys::OrtOpAttr, len: usize) -> Result<Self>
	where
		Self: Sized
	{
		let _ = (attr, len);
		Err(Error::new("not implemented"))
	}

	unsafe fn get_from(info: *mut ort_sys::OrtKernelInfo, name: *const ort_sys::c_char) -> Result<Self>
	where
		Self: Sized;
}

impl GetKernelAttribute<'_> for f32 {
	fn attr_type() -> Option<ort_sys::OrtOpAttrType> {
		Some(ort_sys::OrtOpAttrType::ORT_OP_ATTR_FLOAT)
	}

	unsafe fn from_read_op(attr: *const ort_sys::OrtOpAttr, mut len: usize) -> Result<Self>
	where
		Self: Sized
	{
		let mut out = 0.0_f32;
		ortsys![unsafe ReadOpAttr(attr, ort_sys::OrtOpAttrType::ORT_OP_ATTR_FLOAT, (&mut out as *mut f32).cast(), size_of::<f32>(), &mut len)?];
		assert_eq!(len, size_of::<f32>());
		Ok(out)
	}

	unsafe fn get_from(info: *mut ort_sys::OrtKernelInfo, name: *const ort_sys::c_char) -> Result<Self>
	where
		Self: Sized
	{
		let mut value = Self::default();
		ortsys![unsafe KernelInfoGetAttribute_float(info, name, &mut value)?];
		Ok(value)
	}
}

impl GetKernelAttribute<'_> for i64 {
	fn attr_type() -> Option<ort_sys::OrtOpAttrType> {
		Some(ort_sys::OrtOpAttrType::ORT_OP_ATTR_INT)
	}

	unsafe fn from_read_op(attr: *const ort_sys::OrtOpAttr, mut len: usize) -> Result<Self>
	where
		Self: Sized
	{
		let mut out = 0_i64;
		ortsys![unsafe ReadOpAttr(attr, ort_sys::OrtOpAttrType::ORT_OP_ATTR_INT, (&mut out as *mut i64).cast(), size_of::<i64>(), &mut len)?];
		assert_eq!(len, size_of::<i64>());
		Ok(out)
	}

	unsafe fn get_from(info: *mut ort_sys::OrtKernelInfo, name: *const ort_sys::c_char) -> Result<Self>
	where
		Self: Sized
	{
		let mut value = Self::default();
		ortsys![unsafe KernelInfoGetAttribute_int64(info, name, &mut value)?];
		Ok(value)
	}
}

impl GetKernelAttribute<'_> for String {
	fn attr_type() -> Option<ort_sys::OrtOpAttrType> {
		Some(ort_sys::OrtOpAttrType::ORT_OP_ATTR_STRING)
	}

	unsafe fn from_read_op(attr: *const ort_sys::OrtOpAttr, mut len: usize) -> Result<Self>
	where
		Self: Sized
	{
		let mut out = vec![0_u8; len / size_of::<u8>()];
		ortsys![unsafe ReadOpAttr(attr, ort_sys::OrtOpAttrType::ORT_OP_ATTR_STRING, out.as_mut_ptr().cast(), len, &mut len)?];
		assert_eq!(out.len(), len / size_of::<u8>());
		CString::from_vec_with_nul(out)
			.map_err(|_| Error::new("invalid string"))
			.and_then(|f| f.into_string().map_err(|_| Error::new("invalid string")))
	}

	unsafe fn get_from(info: *mut ort_sys::OrtKernelInfo, name: *const ort_sys::c_char) -> Result<Self>
	where
		Self: Sized
	{
		let mut size = 0;
		ortsys![unsafe KernelInfoGetAttribute_string(info, name, ptr::null_mut(), &mut size)?];
		let mut out = vec![0u8; size];
		ortsys![unsafe KernelInfoGetAttribute_string(info, name, out.as_mut_ptr().cast::<c_char>(), &mut size)?];
		let string = CString::from_vec_with_nul(out)?;
		Ok(string.into_string()?)
	}
}

impl GetKernelAttribute<'_> for Vec<f32> {
	fn attr_type() -> Option<ort_sys::OrtOpAttrType> {
		Some(ort_sys::OrtOpAttrType::ORT_OP_ATTR_FLOATS)
	}

	unsafe fn from_read_op(attr: *const ort_sys::OrtOpAttr, mut len: usize) -> Result<Self>
	where
		Self: Sized
	{
		let mut out = vec![0.0_f32; len / size_of::<f32>()];
		ortsys![unsafe ReadOpAttr(attr, ort_sys::OrtOpAttrType::ORT_OP_ATTR_FLOATS, out.as_mut_ptr().cast(), len, &mut len)?];
		assert_eq!(out.len(), len / size_of::<f32>());
		Ok(out)
	}

	unsafe fn get_from(info: *mut ort_sys::OrtKernelInfo, name: *const ort_sys::c_char) -> Result<Self>
	where
		Self: Sized
	{
		let mut size = 0;
		ortsys![unsafe KernelInfoGetAttributeArray_float(info, name, ptr::null_mut(), &mut size)?];
		let mut out = vec![0f32; size];
		ortsys![unsafe KernelInfoGetAttributeArray_float(info, name, out.as_mut_ptr(), &mut size)?];
		Ok(out)
	}
}

impl GetKernelAttribute<'_> for Vec<i64> {
	fn attr_type() -> Option<ort_sys::OrtOpAttrType> {
		Some(ort_sys::OrtOpAttrType::ORT_OP_ATTR_INTS)
	}

	unsafe fn from_read_op(attr: *const ort_sys::OrtOpAttr, mut len: usize) -> Result<Self>
	where
		Self: Sized
	{
		let mut out = vec![0_i64; len / size_of::<i64>()];
		ortsys![unsafe ReadOpAttr(attr, ort_sys::OrtOpAttrType::ORT_OP_ATTR_INTS, out.as_mut_ptr().cast(), len, &mut len)?];
		assert_eq!(out.len(), len / size_of::<i64>());
		Ok(out)
	}

	unsafe fn get_from(info: *mut ort_sys::OrtKernelInfo, name: *const ort_sys::c_char) -> Result<Self>
	where
		Self: Sized
	{
		let mut size = 0;
		ortsys![unsafe KernelInfoGetAttributeArray_int64(info, name, ptr::null_mut(), &mut size)?];
		let mut out = vec![0i64; size];
		ortsys![unsafe KernelInfoGetAttributeArray_int64(info, name, out.as_mut_ptr(), &mut size)?];
		Ok(out)
	}
}

impl<'s, T: DowncastableTarget> GetKernelAttribute<'s> for ValueRef<'s, T> {
	unsafe fn get_from(info: *mut ort_sys::OrtKernelInfo, name: *const ort_sys::c_char) -> Result<Self>
	where
		Self: Sized
	{
		// TODO: This should probably be customizable - docs say the allocator is required for "internal tensor state", but it's
		// not clear if this also includes tensor data (and thus it should instead be allocated on an appropriate device).
		let allocator = Allocator::default();

		let mut value_ptr: *mut ort_sys::OrtValue = ptr::null_mut();
		ortsys![unsafe KernelInfoGetAttribute_tensor(info, name, allocator.ptr().cast_mut(), &mut value_ptr)?; nonNull(value_ptr)];
		unsafe { ValueRef::new(DynValue::from_ptr(NonNull::new_unchecked(value_ptr), None)) }.downcast()
	}
}

pub struct ScratchBuffer<T> {
	allocator: Allocator,
	buffer: *mut T,
	size: usize
}

impl<T> ScratchBuffer<T> {
	pub unsafe fn as_slice(&self) -> &[T] {
		unsafe { slice::from_raw_parts(self.buffer.cast_const(), self.size) }
	}

	pub unsafe fn as_mut_slice(&mut self) -> &mut [T] {
		unsafe { slice::from_raw_parts_mut(self.buffer, self.size) }
	}
}

impl<T> Drop for ScratchBuffer<T> {
	fn drop(&mut self) {
		unsafe {
			self.allocator.free(self.buffer);
		}
	}
}

pub struct KernelContext {
	ptr: NonNull<ort_sys::OrtKernelContext>
}

impl KernelContext {
	pub(crate) fn new(ctx: *mut ort_sys::OrtKernelContext) -> Self {
		Self {
			ptr: NonNull::from(unsafe { &mut *ctx })
		}
	}

	pub fn input(&self, idx: usize) -> Result<Option<ValueRef<'_>>> {
		let mut value_ptr: *const ort_sys::OrtValue = ptr::null();
		ortsys![unsafe KernelContext_GetInput(self.ptr.as_ptr(), idx, &mut value_ptr)?];
		Ok(NonNull::new(value_ptr.cast_mut()).map(|c| ValueRef::new(unsafe { Value::from_ptr_nodrop(c, None) })))
	}

	pub fn output(&self, idx: usize, shape: impl Into<Shape>) -> Result<Option<ValueRefMut<'_>>> {
		let mut value_ptr: *mut ort_sys::OrtValue = ptr::null_mut();
		let shape = shape.into();
		ortsys![unsafe KernelContext_GetOutput(self.ptr.as_ptr(), idx, shape.as_ptr(), shape.len(), &mut value_ptr)?];
		Ok(NonNull::new(value_ptr).map(|c| ValueRefMut::new(unsafe { Value::from_ptr_nodrop(c, None) })))
	}

	pub fn num_inputs(&self) -> Result<usize> {
		let mut num = 0;
		ortsys![unsafe KernelContext_GetInputCount(self.ptr.as_ptr(), &mut num)?];
		Ok(num)
	}

	pub fn num_outputs(&self) -> Result<usize> {
		let mut num = 0;
		ortsys![unsafe KernelContext_GetOutputCount(self.ptr.as_ptr(), &mut num)?];
		Ok(num)
	}

	pub fn allocator(&self, memory_info: &MemoryInfo) -> Result<Allocator> {
		let mut allocator_ptr = ptr::null_mut();
		ortsys![unsafe KernelContext_GetAllocator(self.ptr.as_ptr(), memory_info.ptr(), &mut allocator_ptr)?];
		Ok(unsafe { Allocator::from_raw_unchecked(allocator_ptr) })
	}

	pub fn get_resource(&self, id: ort_sys::c_int, version: ort_sys::c_int) -> Result<Option<NonNull<ort_sys::c_void>>> {
		let mut resource_ptr: *mut ort_sys::c_void = ptr::null_mut();
		ortsys![unsafe KernelContext_GetResource(self.ptr.as_ptr(), version, id, &mut resource_ptr)?];
		Ok(NonNull::new(resource_ptr))
	}

	pub fn par_for<F>(&self, total: usize, max_num_batches: usize, f: F) -> Result<()>
	where
		F: Fn(usize) + Sync + Send
	{
		let executor = Box::new(f) as Box<dyn Fn(usize) + Sync + Send>;
		ortsys![unsafe KernelContext_ParallelFor(self.ptr.as_ptr(), parallel_for_cb, total, max_num_batches, &executor as *const _ as *mut c_void)?];
		Ok(())
	}

	// TODO: STATUS_ACCESS_VIOLATION inside `KernelContext_GetScratchBuffer`. gonna assume this one is just an internal ONNX
	// Runtime bug.
	//
	// pub fn allocate<T>(&self, memory_info: &MemoryInfo, len: usize) -> Result<ScratchBuffer<T>> {
	// 	let mut buffer = ptr::null_mut();
	// 	let allocator = self.allocator(memory_info)?;
	// 	ortsys![
	// 		unsafe KernelContext_GetScratchBuffer(
	// 			self.ptr.as_ptr(),
	// 			memory_info.ptr.as_ptr(),
	// 			len * core::mem::size_of::<T>(),
	// 			&mut buffer
	// 		)?;
	// 		nonNull(buffer)
	// 	];
	// 	Ok(ScratchBuffer {
	// 		allocator,
	// 		buffer: buffer.cast::<T>(),
	// 		size: len
	// 	})
	// }

	/// Returns a pointer to the GPU compute stream (i.e. `cudaStream_t`) used by the execution provider, if this
	/// kernel's operator was configured to use said execution provider (see
	/// [`super::Operator::execution_provider_type`]).
	pub fn compute_stream(&self) -> Result<Option<NonNull<ort_sys::c_void>>> {
		let mut stream_ptr: *mut ort_sys::c_void = ptr::null_mut();
		ortsys![unsafe KernelContext_GetGPUComputeStream(self.ptr.as_ptr(), &mut stream_ptr)?];
		Ok(NonNull::new(stream_ptr))
	}
}

impl AsPointer for KernelContext {
	type Sys = ort_sys::OrtKernelContext;

	fn ptr(&self) -> *const Self::Sys {
		self.ptr.as_ptr()
	}
}

extern "system" fn parallel_for_cb(user_data: *mut c_void, iterator: usize) {
	let executor = unsafe { &*user_data.cast::<Box<dyn Fn(usize) + Sync + Send>>() };
	executor(iterator)
}
