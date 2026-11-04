pub use root::*;

const _: () = ::planus::check_version_compatibility("planus-1.3.0");


/// The root namespace
/// 
/// Generated from these locations:
/// * File `scene.fbs`
#[no_implicit_prelude]
#[allow(clippy::needless_lifetimes)]
mod root {
/// The namespace `scene`
/// 
/// Generated from these locations:
/// * File `scene.fbs`
pub mod scene {
/// The enum `ColorKind` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Enum `ColorKind` in the file `scene.fbs:6`
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]#[repr(u8)]pub enum ColorKind {
    
        /// The variant `Default` in the enum `ColorKind`
        Default = 0,
    
        /// The variant `Palette` in the enum `ColorKind`
        Palette = 1,
    
        /// The variant `Rgb` in the enum `ColorKind`
        Rgb = 2,
    
}

impl ColorKind {
    /// Array containing all valid variants of ColorKind
    pub const ENUM_VALUES: [Self; 3] = [Self::Default,Self::Palette,Self::Rgb,];
}

impl ::core::convert::TryFrom<u8> for ColorKind {
    type Error = ::planus::errors::UnknownEnumTagKind;
    #[inline]
    fn try_from(value: u8) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTagKind> {
        #[allow(clippy::match_single_binding)]
        match value {
            0 => ::core::result::Result::Ok(ColorKind::Default),
            1 => ::core::result::Result::Ok(ColorKind::Palette),
            2 => ::core::result::Result::Ok(ColorKind::Rgb),
            

            _ => ::core::result::Result::Err(::planus::errors::UnknownEnumTagKind { tag: value as i128 }),
        }
    }
}

impl ::core::convert::From<ColorKind> for u8 {
    #[inline]
    fn from(value: ColorKind) -> Self {
        value as u8
    }
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for ColorKind {
    const ALIGNMENT: usize = 1;
    const SIZE: usize = 1;
}

impl ::planus::WriteAsPrimitive<ColorKind> for ColorKind {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        (*self as u8).write(cursor, buffer_position);
    }
}

impl ::planus::WriteAs<ColorKind> for ColorKind {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ColorKind {
        *self
    }
}

impl ::planus::WriteAsDefault<ColorKind, ColorKind> for ColorKind {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder, default: &ColorKind) -> ::core::option::Option<ColorKind> {
        if self == default {
            ::core::option::Option::None
        } else {
            ::core::option::Option::Some(*self)
        }
    }
}

impl ::planus::WriteAsOptional<ColorKind> for ColorKind {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<ColorKind> {
        ::core::option::Option::Some(*self)
    }
}

impl<'buf> ::planus::TableRead<'buf> for ColorKind {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'buf>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let n: u8 = ::planus::TableRead::from_buffer(buffer, offset)?;
        ::core::result::Result::Ok(::core::convert::TryInto::try_into(n)?)
    }
}

impl<'buf> ::planus::VectorReadInner<'buf> for ColorKind {
    type Error = ::planus::errors::UnknownEnumTag;
    const STRIDE: usize = 1;
    #[inline]
    unsafe fn from_buffer(
        buffer: ::planus::SliceWithStartOffset<'buf>,
        offset: usize,
    ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTag> {let value = unsafe { *buffer.buffer.get_unchecked(offset) };let value: ::core::result::Result<Self, _> = ::core::convert::TryInto::try_into(value);
        value.map_err(|error_kind| error_kind.with_error_location(
            "ColorKind",
            "VectorRead::from_buffer",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<ColorKind> for ColorKind {
    const STRIDE: usize = 1;

    type Value = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Self],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 1];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - i as u32,
                
            );
        }
    }
}

    
/// The enum `CursorShape` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Enum `CursorShape` in the file `scene.fbs:7`
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]#[repr(u8)]pub enum CursorShape {
    
        /// The variant `Bar` in the enum `CursorShape`
        Bar = 0,
    
        /// The variant `Block` in the enum `CursorShape`
        Block = 1,
    
        /// The variant `Underline` in the enum `CursorShape`
        Underline = 2,
    
        /// The variant `Hollow` in the enum `CursorShape`
        Hollow = 3,
    
}

impl CursorShape {
    /// Array containing all valid variants of CursorShape
    pub const ENUM_VALUES: [Self; 4] = [Self::Bar,Self::Block,Self::Underline,Self::Hollow,];
}

impl ::core::convert::TryFrom<u8> for CursorShape {
    type Error = ::planus::errors::UnknownEnumTagKind;
    #[inline]
    fn try_from(value: u8) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTagKind> {
        #[allow(clippy::match_single_binding)]
        match value {
            0 => ::core::result::Result::Ok(CursorShape::Bar),
            1 => ::core::result::Result::Ok(CursorShape::Block),
            2 => ::core::result::Result::Ok(CursorShape::Underline),
            3 => ::core::result::Result::Ok(CursorShape::Hollow),
            

            _ => ::core::result::Result::Err(::planus::errors::UnknownEnumTagKind { tag: value as i128 }),
        }
    }
}

impl ::core::convert::From<CursorShape> for u8 {
    #[inline]
    fn from(value: CursorShape) -> Self {
        value as u8
    }
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for CursorShape {
    const ALIGNMENT: usize = 1;
    const SIZE: usize = 1;
}

impl ::planus::WriteAsPrimitive<CursorShape> for CursorShape {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        (*self as u8).write(cursor, buffer_position);
    }
}

impl ::planus::WriteAs<CursorShape> for CursorShape {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> CursorShape {
        *self
    }
}

impl ::planus::WriteAsDefault<CursorShape, CursorShape> for CursorShape {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder, default: &CursorShape) -> ::core::option::Option<CursorShape> {
        if self == default {
            ::core::option::Option::None
        } else {
            ::core::option::Option::Some(*self)
        }
    }
}

impl ::planus::WriteAsOptional<CursorShape> for CursorShape {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<CursorShape> {
        ::core::option::Option::Some(*self)
    }
}

impl<'buf> ::planus::TableRead<'buf> for CursorShape {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'buf>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let n: u8 = ::planus::TableRead::from_buffer(buffer, offset)?;
        ::core::result::Result::Ok(::core::convert::TryInto::try_into(n)?)
    }
}

impl<'buf> ::planus::VectorReadInner<'buf> for CursorShape {
    type Error = ::planus::errors::UnknownEnumTag;
    const STRIDE: usize = 1;
    #[inline]
    unsafe fn from_buffer(
        buffer: ::planus::SliceWithStartOffset<'buf>,
        offset: usize,
    ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTag> {let value = unsafe { *buffer.buffer.get_unchecked(offset) };let value: ::core::result::Result<Self, _> = ::core::convert::TryInto::try_into(value);
        value.map_err(|error_kind| error_kind.with_error_location(
            "CursorShape",
            "VectorRead::from_buffer",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<CursorShape> for CursorShape {
    const STRIDE: usize = 1;

    type Value = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Self],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 1];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - i as u32,
                
            );
        }
    }
}

    
/// The enum `PixelFormat` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Enum `PixelFormat` in the file `scene.fbs:8`
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]#[repr(u8)]pub enum PixelFormat {
    
        /// The variant `Rgb` in the enum `PixelFormat`
        Rgb = 0,
    
        /// The variant `Rgba` in the enum `PixelFormat`
        Rgba = 1,
    
        /// The variant `GrayAlpha` in the enum `PixelFormat`
        GrayAlpha = 2,
    
        /// The variant `Gray` in the enum `PixelFormat`
        Gray = 3,
    
}

impl PixelFormat {
    /// Array containing all valid variants of PixelFormat
    pub const ENUM_VALUES: [Self; 4] = [Self::Rgb,Self::Rgba,Self::GrayAlpha,Self::Gray,];
}

impl ::core::convert::TryFrom<u8> for PixelFormat {
    type Error = ::planus::errors::UnknownEnumTagKind;
    #[inline]
    fn try_from(value: u8) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTagKind> {
        #[allow(clippy::match_single_binding)]
        match value {
            0 => ::core::result::Result::Ok(PixelFormat::Rgb),
            1 => ::core::result::Result::Ok(PixelFormat::Rgba),
            2 => ::core::result::Result::Ok(PixelFormat::GrayAlpha),
            3 => ::core::result::Result::Ok(PixelFormat::Gray),
            

            _ => ::core::result::Result::Err(::planus::errors::UnknownEnumTagKind { tag: value as i128 }),
        }
    }
}

impl ::core::convert::From<PixelFormat> for u8 {
    #[inline]
    fn from(value: PixelFormat) -> Self {
        value as u8
    }
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for PixelFormat {
    const ALIGNMENT: usize = 1;
    const SIZE: usize = 1;
}

impl ::planus::WriteAsPrimitive<PixelFormat> for PixelFormat {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        (*self as u8).write(cursor, buffer_position);
    }
}

impl ::planus::WriteAs<PixelFormat> for PixelFormat {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> PixelFormat {
        *self
    }
}

impl ::planus::WriteAsDefault<PixelFormat, PixelFormat> for PixelFormat {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder, default: &PixelFormat) -> ::core::option::Option<PixelFormat> {
        if self == default {
            ::core::option::Option::None
        } else {
            ::core::option::Option::Some(*self)
        }
    }
}

impl ::planus::WriteAsOptional<PixelFormat> for PixelFormat {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<PixelFormat> {
        ::core::option::Option::Some(*self)
    }
}

impl<'buf> ::planus::TableRead<'buf> for PixelFormat {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'buf>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let n: u8 = ::planus::TableRead::from_buffer(buffer, offset)?;
        ::core::result::Result::Ok(::core::convert::TryInto::try_into(n)?)
    }
}

impl<'buf> ::planus::VectorReadInner<'buf> for PixelFormat {
    type Error = ::planus::errors::UnknownEnumTag;
    const STRIDE: usize = 1;
    #[inline]
    unsafe fn from_buffer(
        buffer: ::planus::SliceWithStartOffset<'buf>,
        offset: usize,
    ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTag> {let value = unsafe { *buffer.buffer.get_unchecked(offset) };let value: ::core::result::Result<Self, _> = ::core::convert::TryInto::try_into(value);
        value.map_err(|error_kind| error_kind.with_error_location(
            "PixelFormat",
            "VectorRead::from_buffer",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<PixelFormat> for PixelFormat {
    const STRIDE: usize = 1;

    type Value = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Self],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 1];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - i as u32,
                
            );
        }
    }
}

    
/// The enum `ImageLayer` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Enum `ImageLayer` in the file `scene.fbs:9`
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]#[repr(u8)]pub enum ImageLayer {
    
        /// The variant `BelowBackground` in the enum `ImageLayer`
        BelowBackground = 0,
    
        /// The variant `BelowText` in the enum `ImageLayer`
        BelowText = 1,
    
        /// The variant `AboveText` in the enum `ImageLayer`
        AboveText = 2,
    
}

impl ImageLayer {
    /// Array containing all valid variants of ImageLayer
    pub const ENUM_VALUES: [Self; 3] = [Self::BelowBackground,Self::BelowText,Self::AboveText,];
}

impl ::core::convert::TryFrom<u8> for ImageLayer {
    type Error = ::planus::errors::UnknownEnumTagKind;
    #[inline]
    fn try_from(value: u8) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTagKind> {
        #[allow(clippy::match_single_binding)]
        match value {
            0 => ::core::result::Result::Ok(ImageLayer::BelowBackground),
            1 => ::core::result::Result::Ok(ImageLayer::BelowText),
            2 => ::core::result::Result::Ok(ImageLayer::AboveText),
            

            _ => ::core::result::Result::Err(::planus::errors::UnknownEnumTagKind { tag: value as i128 }),
        }
    }
}

impl ::core::convert::From<ImageLayer> for u8 {
    #[inline]
    fn from(value: ImageLayer) -> Self {
        value as u8
    }
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for ImageLayer {
    const ALIGNMENT: usize = 1;
    const SIZE: usize = 1;
}

impl ::planus::WriteAsPrimitive<ImageLayer> for ImageLayer {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        (*self as u8).write(cursor, buffer_position);
    }
}

impl ::planus::WriteAs<ImageLayer> for ImageLayer {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ImageLayer {
        *self
    }
}

impl ::planus::WriteAsDefault<ImageLayer, ImageLayer> for ImageLayer {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder, default: &ImageLayer) -> ::core::option::Option<ImageLayer> {
        if self == default {
            ::core::option::Option::None
        } else {
            ::core::option::Option::Some(*self)
        }
    }
}

impl ::planus::WriteAsOptional<ImageLayer> for ImageLayer {
    type Prepared = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<ImageLayer> {
        ::core::option::Option::Some(*self)
    }
}

impl<'buf> ::planus::TableRead<'buf> for ImageLayer {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'buf>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let n: u8 = ::planus::TableRead::from_buffer(buffer, offset)?;
        ::core::result::Result::Ok(::core::convert::TryInto::try_into(n)?)
    }
}

impl<'buf> ::planus::VectorReadInner<'buf> for ImageLayer {
    type Error = ::planus::errors::UnknownEnumTag;
    const STRIDE: usize = 1;
    #[inline]
    unsafe fn from_buffer(
        buffer: ::planus::SliceWithStartOffset<'buf>,
        offset: usize,
    ) -> ::core::result::Result<Self, ::planus::errors::UnknownEnumTag> {let value = unsafe { *buffer.buffer.get_unchecked(offset) };let value: ::core::result::Result<Self, _> = ::core::convert::TryInto::try_into(value);
        value.map_err(|error_kind| error_kind.with_error_location(
            "ImageLayer",
            "VectorRead::from_buffer",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<ImageLayer> for ImageLayer {
    const STRIDE: usize = 1;

    type Value = Self;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Self],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 1];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - i as u32,
                
            );
        }
    }
}

    
/// The struct `Size` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Struct `Size` in the file `scene.fbs:11`
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
Default, 
::serde::Serialize, ::serde::Deserialize
)]
pub struct Size {
    
        /// The field `cols` in the struct `Size`
        pub cols: u16,
    
        /// The field `rows` in the struct `Size`
        pub rows: u16,
    
        /// The field `cell_width` in the struct `Size`
        pub cell_width: u32,
    
        /// The field `cell_height` in the struct `Size`
        pub cell_height: u32,
    
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for Size {
    const ALIGNMENT: usize = 4;
    const SIZE: usize = 12;
}

#[allow(clippy::identity_op)]
impl ::planus::WriteAsPrimitive<Size> for Size {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        let (cur, cursor) = cursor.split::<2 , 10>();
            self.cols.write(cur, buffer_position - 0);let (cur, cursor) = cursor.split::<2 , 8>();
            self.rows.write(cur, buffer_position - 2);let (cur, cursor) = cursor.split::<4 , 4>();
            self.cell_width.write(cur, buffer_position - 4);let (cur, cursor) = cursor.split::<4 , 0>();
            self.cell_height.write(cur, buffer_position - 8);
        cursor.finish([]);
    }
}

impl ::planus::WriteAsOffset<Size> for Size {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Size> {
        unsafe {
            builder.write_with(
                12,
                3,
                |buffer_position, bytes| {
                    let bytes = bytes.as_mut_ptr();

                    ::planus::WriteAsPrimitive::write(
                        self,
                        ::planus::Cursor::new(&mut *(bytes as *mut [::core::mem::MaybeUninit<u8>; 12])),
                        buffer_position,
                    );
                }
            );
        }
        builder.current_offset()
    }
}

impl ::planus::WriteAs<Size> for Size {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }
}

impl ::planus::WriteAsOptional<Size> for Size {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<Self> {
        ::core::option::Option::Some(*self)
    }
}

/// Reference to a deserialized [Size].
#[derive(Copy, Clone)]
pub struct SizeRef<'a>(::planus::ArrayWithStartOffset<'a, 12>);

impl<'a> SizeRef<'a> {
    
        /// Getter for the [`cols` field](Size#structfield.cols).
        pub fn cols(&self) -> u16 {
            let buffer = self.0.advance_as_array::<2>(0).unwrap();
            
            u16::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`rows` field](Size#structfield.rows).
        pub fn rows(&self) -> u16 {
            let buffer = self.0.advance_as_array::<2>(2).unwrap();
            
            u16::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`cell_width` field](Size#structfield.cell_width).
        pub fn cell_width(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(4).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`cell_height` field](Size#structfield.cell_height).
        pub fn cell_height(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(8).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
}

impl<'a> ::core::fmt::Debug for SizeRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("SizeRef");
        f.field("cols", &self.cols());f.field("rows", &self.rows());f.field("cell_width", &self.cell_width());f.field("cell_height", &self.cell_height());
        f.finish()
    }
}

impl<'a> ::core::convert::From<::planus::ArrayWithStartOffset<'a, 12>> for SizeRef<'a> {
    fn from(array: ::planus::ArrayWithStartOffset<'a, 12>) -> Self {
        Self(array)
    }
}

impl<'a> ::core::convert::From<SizeRef<'a>> for Size {
    #[allow(unreachable_code)]
    fn from(value: SizeRef<'a>) -> Self {
        Self {
        cols:
            value.cols(),
            rows:
            value.rows(),
            cell_width:
            value.cell_width(),
            cell_height:
            value.cell_height(),
            
        }
    }
}

impl<'a, 'b> ::core::cmp::PartialEq<SizeRef<'a>> for SizeRef<'b> {
    fn eq(&self, other: &SizeRef<'_>) -> bool {
        
            self.cols() == other.cols() && 
            self.rows() == other.rows() && 
            self.cell_width() == other.cell_width() && 
            self.cell_height() == other.cell_height()
    }
}

    impl<'a> ::core::cmp::Eq for SizeRef<'a> {}
    impl<'a, 'b> ::core::cmp::PartialOrd<SizeRef<'a>> for SizeRef<'b> {
        fn partial_cmp(&self, other: &SizeRef<'_>) -> ::core::option::Option<::core::cmp::Ordering> {
            ::core::option::Option::Some(::core::cmp::Ord::cmp(self, other))
        }
    }

    impl<'a> ::core::cmp::Ord for SizeRef<'a> {
        fn cmp(&self, other: &SizeRef<'_>) -> ::core::cmp::Ordering {
            
                self.cols().cmp(&other.cols())
                
                .then_with(|| self.rows().cmp(&other.rows()))
                
                .then_with(|| self.cell_width().cmp(&other.cell_width()))
                
                .then_with(|| self.cell_height().cmp(&other.cell_height()))
                
        }
    }

    impl<'a> ::core::hash::Hash for SizeRef<'a> {
        fn hash<H: ::core::hash::Hasher>(&self, state: &mut H) {
            self.cols().hash(state);self.rows().hash(state);self.cell_width().hash(state);self.cell_height().hash(state);
        }
    }


impl<'a> ::planus::TableRead<'a> for SizeRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let buffer = buffer.advance_as_array::<12>(offset)?;
        ::core::result::Result::Ok(Self(buffer))
    }
}

impl<'a> ::planus::VectorRead<'a> for SizeRef<'a> {
    const STRIDE: usize = 12;

    #[inline]
    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> Self {
        Self(unsafe { buffer.unchecked_advance_as_array(offset) })
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<Size> for Size {
    const STRIDE: usize = 12;

    type Value = Size;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self::Value {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Size],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 12];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - (12 * i) as u32,
                
            );
        }
    }
}

    
/// The struct `Style` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Struct `Style` in the file `scene.fbs:19`
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,

::serde::Serialize, ::serde::Deserialize
)]
pub struct Style {
    
        /// The field `foreground` in the struct `Style`
        pub foreground: u32,
    
        /// The field `background` in the struct `Style`
        pub background: u32,
    
        /// The field `underline_color` in the struct `Style`
        pub underline_color: u32,
    
        /// The field `foreground_kind` in the struct `Style`
        pub foreground_kind: self::ColorKind,
    
        /// The field `foreground_value` in the struct `Style`
        pub foreground_value: u32,
    
        /// The field `background_kind` in the struct `Style`
        pub background_kind: self::ColorKind,
    
        /// The field `background_value` in the struct `Style`
        pub background_value: u32,
    
        /// The field `underline_kind` in the struct `Style`
        pub underline_kind: self::ColorKind,
    
        /// The field `underline_value` in the struct `Style`
        pub underline_value: u32,
    
        /// The field `flags` in the struct `Style`
        pub flags: u16,
    
        /// The field `underline` in the struct `Style`
        pub underline: u8,
    
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for Style {
    const ALIGNMENT: usize = 4;
    const SIZE: usize = 40;
}

#[allow(clippy::identity_op)]
impl ::planus::WriteAsPrimitive<Style> for Style {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        let (cur, cursor) = cursor.split::<4 , 36>();
            self.foreground.write(cur, buffer_position - 0);let (cur, cursor) = cursor.split::<4 , 32>();
            self.background.write(cur, buffer_position - 4);let (cur, cursor) = cursor.split::<4 , 28>();
            self.underline_color.write(cur, buffer_position - 8);let (cur, cursor) = cursor.split::<1 , 27>();
            self.foreground_kind.write(cur, buffer_position - 12);let cursor = cursor.write::<3, 24>([0; 3]);let (cur, cursor) = cursor.split::<4 , 20>();
            self.foreground_value.write(cur, buffer_position - 16);let (cur, cursor) = cursor.split::<1 , 19>();
            self.background_kind.write(cur, buffer_position - 20);let cursor = cursor.write::<3, 16>([0; 3]);let (cur, cursor) = cursor.split::<4 , 12>();
            self.background_value.write(cur, buffer_position - 24);let (cur, cursor) = cursor.split::<1 , 11>();
            self.underline_kind.write(cur, buffer_position - 28);let cursor = cursor.write::<3, 8>([0; 3]);let (cur, cursor) = cursor.split::<4 , 4>();
            self.underline_value.write(cur, buffer_position - 32);let (cur, cursor) = cursor.split::<2 , 2>();
            self.flags.write(cur, buffer_position - 36);let (cur, cursor) = cursor.split::<1 , 1>();
            self.underline.write(cur, buffer_position - 38);let cursor = cursor.write::<1, 0>([0; 1]);
        cursor.finish([]);
    }
}

impl ::planus::WriteAsOffset<Style> for Style {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Style> {
        unsafe {
            builder.write_with(
                40,
                3,
                |buffer_position, bytes| {
                    let bytes = bytes.as_mut_ptr();

                    ::planus::WriteAsPrimitive::write(
                        self,
                        ::planus::Cursor::new(&mut *(bytes as *mut [::core::mem::MaybeUninit<u8>; 40])),
                        buffer_position,
                    );
                }
            );
        }
        builder.current_offset()
    }
}

impl ::planus::WriteAs<Style> for Style {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }
}

impl ::planus::WriteAsOptional<Style> for Style {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<Self> {
        ::core::option::Option::Some(*self)
    }
}

/// Reference to a deserialized [Style].
#[derive(Copy, Clone)]
pub struct StyleRef<'a>(::planus::ArrayWithStartOffset<'a, 40>);

impl<'a> StyleRef<'a> {
    
        /// Getter for the [`foreground` field](Style#structfield.foreground).
        pub fn foreground(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(0).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`background` field](Style#structfield.background).
        pub fn background(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(4).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`underline_color` field](Style#structfield.underline_color).
        pub fn underline_color(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(8).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`foreground_kind` field](Style#structfield.foreground_kind).
        pub fn foreground_kind(&self) -> ::core::result::Result<self::ColorKind, ::planus::errors::UnknownEnumTag> {
            let buffer = self.0.advance_as_array::<1>(12).unwrap();
            
            let value: ::core::result::Result<self::ColorKind, _> = ::core::convert::TryInto::try_into(u8::from_le_bytes(*buffer.as_array()));
                    value.map_err(|e| e.with_error_location(
                        "StyleRef",
                        "foreground_kind",
                        buffer.offset_from_start,
                    ))
            
        }
    
        /// Getter for the [`foreground_value` field](Style#structfield.foreground_value).
        pub fn foreground_value(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(16).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`background_kind` field](Style#structfield.background_kind).
        pub fn background_kind(&self) -> ::core::result::Result<self::ColorKind, ::planus::errors::UnknownEnumTag> {
            let buffer = self.0.advance_as_array::<1>(20).unwrap();
            
            let value: ::core::result::Result<self::ColorKind, _> = ::core::convert::TryInto::try_into(u8::from_le_bytes(*buffer.as_array()));
                    value.map_err(|e| e.with_error_location(
                        "StyleRef",
                        "background_kind",
                        buffer.offset_from_start,
                    ))
            
        }
    
        /// Getter for the [`background_value` field](Style#structfield.background_value).
        pub fn background_value(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(24).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`underline_kind` field](Style#structfield.underline_kind).
        pub fn underline_kind(&self) -> ::core::result::Result<self::ColorKind, ::planus::errors::UnknownEnumTag> {
            let buffer = self.0.advance_as_array::<1>(28).unwrap();
            
            let value: ::core::result::Result<self::ColorKind, _> = ::core::convert::TryInto::try_into(u8::from_le_bytes(*buffer.as_array()));
                    value.map_err(|e| e.with_error_location(
                        "StyleRef",
                        "underline_kind",
                        buffer.offset_from_start,
                    ))
            
        }
    
        /// Getter for the [`underline_value` field](Style#structfield.underline_value).
        pub fn underline_value(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(32).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`flags` field](Style#structfield.flags).
        pub fn flags(&self) -> u16 {
            let buffer = self.0.advance_as_array::<2>(36).unwrap();
            
            u16::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`underline` field](Style#structfield.underline).
        pub fn underline(&self) -> u8 {
            let buffer = self.0.advance_as_array::<1>(38).unwrap();
            
            u8::from_le_bytes(*buffer.as_array())
            
        }
    
}

impl<'a> ::core::fmt::Debug for StyleRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("StyleRef");
        f.field("foreground", &self.foreground());f.field("background", &self.background());f.field("underline_color", &self.underline_color());f.field("foreground_kind", &self.foreground_kind());f.field("foreground_value", &self.foreground_value());f.field("background_kind", &self.background_kind());f.field("background_value", &self.background_value());f.field("underline_kind", &self.underline_kind());f.field("underline_value", &self.underline_value());f.field("flags", &self.flags());f.field("underline", &self.underline());
        f.finish()
    }
}

impl<'a> ::core::convert::From<::planus::ArrayWithStartOffset<'a, 40>> for StyleRef<'a> {
    fn from(array: ::planus::ArrayWithStartOffset<'a, 40>) -> Self {
        Self(array)
    }
}

impl<'a> ::core::convert::TryFrom<StyleRef<'a>> for Style {
    type Error = ::planus::Error;

    #[allow(unreachable_code)]
    fn try_from(value: StyleRef<'a>) -> ::planus::Result<Self> {
        ::core::result::Result::Ok(Self {
        foreground:
            value.foreground(),
            background:
            value.background(),
            underline_color:
            value.underline_color(),
            foreground_kind:
            value.foreground_kind()?,foreground_value:
            value.foreground_value(),
            background_kind:
            value.background_kind()?,background_value:
            value.background_value(),
            underline_kind:
            value.underline_kind()?,underline_value:
            value.underline_value(),
            flags:
            value.flags(),
            underline:
            value.underline(),
            
        })
    }
}

impl<'a> ::planus::TableRead<'a> for StyleRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let buffer = buffer.advance_as_array::<40>(offset)?;
        ::core::result::Result::Ok(Self(buffer))
    }
}

impl<'a> ::planus::VectorRead<'a> for StyleRef<'a> {
    const STRIDE: usize = 40;

    #[inline]
    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> Self {
        Self(unsafe { buffer.unchecked_advance_as_array(offset) })
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<Style> for Style {
    const STRIDE: usize = 40;

    type Value = Style;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self::Value {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Style],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 40];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - (40 * i) as u32,
                
            );
        }
    }
}

    
/// The struct `Layout` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Struct `Layout` in the file `scene.fbs:33`
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
Default, 
::serde::Serialize, ::serde::Deserialize
)]
pub struct Layout {
    
        /// The field `length` in the struct `Layout`
        pub length: u32,
    
        /// The field `width` in the struct `Layout`
        pub width: u8,
    
        /// The field `count` in the struct `Layout`
        pub count: u32,
    
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for Layout {
    const ALIGNMENT: usize = 4;
    const SIZE: usize = 12;
}

#[allow(clippy::identity_op)]
impl ::planus::WriteAsPrimitive<Layout> for Layout {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        let (cur, cursor) = cursor.split::<4 , 8>();
            self.length.write(cur, buffer_position - 0);let (cur, cursor) = cursor.split::<1 , 7>();
            self.width.write(cur, buffer_position - 4);let cursor = cursor.write::<3, 4>([0; 3]);let (cur, cursor) = cursor.split::<4 , 0>();
            self.count.write(cur, buffer_position - 8);
        cursor.finish([]);
    }
}

impl ::planus::WriteAsOffset<Layout> for Layout {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Layout> {
        unsafe {
            builder.write_with(
                12,
                3,
                |buffer_position, bytes| {
                    let bytes = bytes.as_mut_ptr();

                    ::planus::WriteAsPrimitive::write(
                        self,
                        ::planus::Cursor::new(&mut *(bytes as *mut [::core::mem::MaybeUninit<u8>; 12])),
                        buffer_position,
                    );
                }
            );
        }
        builder.current_offset()
    }
}

impl ::planus::WriteAs<Layout> for Layout {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }
}

impl ::planus::WriteAsOptional<Layout> for Layout {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<Self> {
        ::core::option::Option::Some(*self)
    }
}

/// Reference to a deserialized [Layout].
#[derive(Copy, Clone)]
pub struct LayoutRef<'a>(::planus::ArrayWithStartOffset<'a, 12>);

impl<'a> LayoutRef<'a> {
    
        /// Getter for the [`length` field](Layout#structfield.length).
        pub fn length(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(0).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`width` field](Layout#structfield.width).
        pub fn width(&self) -> u8 {
            let buffer = self.0.advance_as_array::<1>(4).unwrap();
            
            u8::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`count` field](Layout#structfield.count).
        pub fn count(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(8).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
}

impl<'a> ::core::fmt::Debug for LayoutRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("LayoutRef");
        f.field("length", &self.length());f.field("width", &self.width());f.field("count", &self.count());
        f.finish()
    }
}

impl<'a> ::core::convert::From<::planus::ArrayWithStartOffset<'a, 12>> for LayoutRef<'a> {
    fn from(array: ::planus::ArrayWithStartOffset<'a, 12>) -> Self {
        Self(array)
    }
}

impl<'a> ::core::convert::From<LayoutRef<'a>> for Layout {
    #[allow(unreachable_code)]
    fn from(value: LayoutRef<'a>) -> Self {
        Self {
        length:
            value.length(),
            width:
            value.width(),
            count:
            value.count(),
            
        }
    }
}

impl<'a, 'b> ::core::cmp::PartialEq<LayoutRef<'a>> for LayoutRef<'b> {
    fn eq(&self, other: &LayoutRef<'_>) -> bool {
        
            self.length() == other.length() && 
            self.width() == other.width() && 
            self.count() == other.count()
    }
}

    impl<'a> ::core::cmp::Eq for LayoutRef<'a> {}
    impl<'a, 'b> ::core::cmp::PartialOrd<LayoutRef<'a>> for LayoutRef<'b> {
        fn partial_cmp(&self, other: &LayoutRef<'_>) -> ::core::option::Option<::core::cmp::Ordering> {
            ::core::option::Option::Some(::core::cmp::Ord::cmp(self, other))
        }
    }

    impl<'a> ::core::cmp::Ord for LayoutRef<'a> {
        fn cmp(&self, other: &LayoutRef<'_>) -> ::core::cmp::Ordering {
            
                self.length().cmp(&other.length())
                
                .then_with(|| self.width().cmp(&other.width()))
                
                .then_with(|| self.count().cmp(&other.count()))
                
        }
    }

    impl<'a> ::core::hash::Hash for LayoutRef<'a> {
        fn hash<H: ::core::hash::Hasher>(&self, state: &mut H) {
            self.length().hash(state);self.width().hash(state);self.count().hash(state);
        }
    }


impl<'a> ::planus::TableRead<'a> for LayoutRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let buffer = buffer.advance_as_array::<12>(offset)?;
        ::core::result::Result::Ok(Self(buffer))
    }
}

impl<'a> ::planus::VectorRead<'a> for LayoutRef<'a> {
    const STRIDE: usize = 12;

    #[inline]
    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> Self {
        Self(unsafe { buffer.unchecked_advance_as_array(offset) })
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<Layout> for Layout {
    const STRIDE: usize = 12;

    type Value = Layout;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self::Value {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Layout],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 12];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - (12 * i) as u32,
                
            );
        }
    }
}

    
/// The struct `Placement` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Struct `Placement` in the file `scene.fbs:39`
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,

::serde::Serialize, ::serde::Deserialize
)]
pub struct Placement {
    
        /// The field `image_id` in the struct `Placement`
        pub image_id: u32,
    
        /// The field `generation` in the struct `Placement`
        pub generation: u64,
    
        /// The field `placement_id` in the struct `Placement`
        pub placement_id: u32,
    
        /// The field `col` in the struct `Placement`
        pub col: i32,
    
        /// The field `row` in the struct `Placement`
        pub row: i32,
    
        /// The field `offset_x` in the struct `Placement`
        pub offset_x: u32,
    
        /// The field `offset_y` in the struct `Placement`
        pub offset_y: u32,
    
        /// The field `width` in the struct `Placement`
        pub width: u32,
    
        /// The field `height` in the struct `Placement`
        pub height: u32,
    
        /// The field `source_x` in the struct `Placement`
        pub source_x: u32,
    
        /// The field `source_y` in the struct `Placement`
        pub source_y: u32,
    
        /// The field `source_width` in the struct `Placement`
        pub source_width: u32,
    
        /// The field `source_height` in the struct `Placement`
        pub source_height: u32,
    
        /// The field `layer` in the struct `Placement`
        pub layer: self::ImageLayer,
    
        /// The field `z` in the struct `Placement`
        pub z: i32,
    
}

/// # Safety
/// The Planus compiler correctly calculates `ALIGNMENT` and `SIZE`.
unsafe impl ::planus::Primitive for Placement {
    const ALIGNMENT: usize = 8;
    const SIZE: usize = 72;
}

#[allow(clippy::identity_op)]
impl ::planus::WriteAsPrimitive<Placement> for Placement {
    #[inline]
    fn write<const N: usize>(&self, cursor: ::planus::Cursor<'_, N>, buffer_position: u32) {
        let (cur, cursor) = cursor.split::<4 , 68>();
            self.image_id.write(cur, buffer_position - 0);let cursor = cursor.write::<4, 64>([0; 4]);let (cur, cursor) = cursor.split::<8 , 56>();
            self.generation.write(cur, buffer_position - 8);let (cur, cursor) = cursor.split::<4 , 52>();
            self.placement_id.write(cur, buffer_position - 16);let (cur, cursor) = cursor.split::<4 , 48>();
            self.col.write(cur, buffer_position - 20);let (cur, cursor) = cursor.split::<4 , 44>();
            self.row.write(cur, buffer_position - 24);let (cur, cursor) = cursor.split::<4 , 40>();
            self.offset_x.write(cur, buffer_position - 28);let (cur, cursor) = cursor.split::<4 , 36>();
            self.offset_y.write(cur, buffer_position - 32);let (cur, cursor) = cursor.split::<4 , 32>();
            self.width.write(cur, buffer_position - 36);let (cur, cursor) = cursor.split::<4 , 28>();
            self.height.write(cur, buffer_position - 40);let (cur, cursor) = cursor.split::<4 , 24>();
            self.source_x.write(cur, buffer_position - 44);let (cur, cursor) = cursor.split::<4 , 20>();
            self.source_y.write(cur, buffer_position - 48);let (cur, cursor) = cursor.split::<4 , 16>();
            self.source_width.write(cur, buffer_position - 52);let (cur, cursor) = cursor.split::<4 , 12>();
            self.source_height.write(cur, buffer_position - 56);let (cur, cursor) = cursor.split::<1 , 11>();
            self.layer.write(cur, buffer_position - 60);let cursor = cursor.write::<3, 8>([0; 3]);let (cur, cursor) = cursor.split::<4 , 4>();
            self.z.write(cur, buffer_position - 64);let cursor = cursor.write::<4, 0>([0; 4]);
        cursor.finish([]);
    }
}

impl ::planus::WriteAsOffset<Placement> for Placement {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Placement> {
        unsafe {
            builder.write_with(
                72,
                7,
                |buffer_position, bytes| {
                    let bytes = bytes.as_mut_ptr();

                    ::planus::WriteAsPrimitive::write(
                        self,
                        ::planus::Cursor::new(&mut *(bytes as *mut [::core::mem::MaybeUninit<u8>; 72])),
                        buffer_position,
                    );
                }
            );
        }
        builder.current_offset()
    }
}

impl ::planus::WriteAs<Placement> for Placement {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self {
        *self
    }
}

impl ::planus::WriteAsOptional<Placement> for Placement {
    type Prepared = Self;
    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> ::core::option::Option<Self> {
        ::core::option::Option::Some(*self)
    }
}

/// Reference to a deserialized [Placement].
#[derive(Copy, Clone)]
pub struct PlacementRef<'a>(::planus::ArrayWithStartOffset<'a, 72>);

impl<'a> PlacementRef<'a> {
    
        /// Getter for the [`image_id` field](Placement#structfield.image_id).
        pub fn image_id(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(0).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`generation` field](Placement#structfield.generation).
        pub fn generation(&self) -> u64 {
            let buffer = self.0.advance_as_array::<8>(8).unwrap();
            
            u64::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`placement_id` field](Placement#structfield.placement_id).
        pub fn placement_id(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(16).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`col` field](Placement#structfield.col).
        pub fn col(&self) -> i32 {
            let buffer = self.0.advance_as_array::<4>(20).unwrap();
            
            i32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`row` field](Placement#structfield.row).
        pub fn row(&self) -> i32 {
            let buffer = self.0.advance_as_array::<4>(24).unwrap();
            
            i32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`offset_x` field](Placement#structfield.offset_x).
        pub fn offset_x(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(28).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`offset_y` field](Placement#structfield.offset_y).
        pub fn offset_y(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(32).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`width` field](Placement#structfield.width).
        pub fn width(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(36).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`height` field](Placement#structfield.height).
        pub fn height(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(40).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`source_x` field](Placement#structfield.source_x).
        pub fn source_x(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(44).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`source_y` field](Placement#structfield.source_y).
        pub fn source_y(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(48).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`source_width` field](Placement#structfield.source_width).
        pub fn source_width(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(52).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`source_height` field](Placement#structfield.source_height).
        pub fn source_height(&self) -> u32 {
            let buffer = self.0.advance_as_array::<4>(56).unwrap();
            
            u32::from_le_bytes(*buffer.as_array())
            
        }
    
        /// Getter for the [`layer` field](Placement#structfield.layer).
        pub fn layer(&self) -> ::core::result::Result<self::ImageLayer, ::planus::errors::UnknownEnumTag> {
            let buffer = self.0.advance_as_array::<1>(60).unwrap();
            
            let value: ::core::result::Result<self::ImageLayer, _> = ::core::convert::TryInto::try_into(u8::from_le_bytes(*buffer.as_array()));
                    value.map_err(|e| e.with_error_location(
                        "PlacementRef",
                        "layer",
                        buffer.offset_from_start,
                    ))
            
        }
    
        /// Getter for the [`z` field](Placement#structfield.z).
        pub fn z(&self) -> i32 {
            let buffer = self.0.advance_as_array::<4>(64).unwrap();
            
            i32::from_le_bytes(*buffer.as_array())
            
        }
    
}

impl<'a> ::core::fmt::Debug for PlacementRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("PlacementRef");
        f.field("image_id", &self.image_id());f.field("generation", &self.generation());f.field("placement_id", &self.placement_id());f.field("col", &self.col());f.field("row", &self.row());f.field("offset_x", &self.offset_x());f.field("offset_y", &self.offset_y());f.field("width", &self.width());f.field("height", &self.height());f.field("source_x", &self.source_x());f.field("source_y", &self.source_y());f.field("source_width", &self.source_width());f.field("source_height", &self.source_height());f.field("layer", &self.layer());f.field("z", &self.z());
        f.finish()
    }
}

impl<'a> ::core::convert::From<::planus::ArrayWithStartOffset<'a, 72>> for PlacementRef<'a> {
    fn from(array: ::planus::ArrayWithStartOffset<'a, 72>) -> Self {
        Self(array)
    }
}

impl<'a> ::core::convert::TryFrom<PlacementRef<'a>> for Placement {
    type Error = ::planus::Error;

    #[allow(unreachable_code)]
    fn try_from(value: PlacementRef<'a>) -> ::planus::Result<Self> {
        ::core::result::Result::Ok(Self {
        image_id:
            value.image_id(),
            generation:
            value.generation(),
            placement_id:
            value.placement_id(),
            col:
            value.col(),
            row:
            value.row(),
            offset_x:
            value.offset_x(),
            offset_y:
            value.offset_y(),
            width:
            value.width(),
            height:
            value.height(),
            source_x:
            value.source_x(),
            source_y:
            value.source_y(),
            source_width:
            value.source_width(),
            source_height:
            value.source_height(),
            layer:
            value.layer()?,z:
            value.z(),
            
        })
    }
}

impl<'a> ::planus::TableRead<'a> for PlacementRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        let buffer = buffer.advance_as_array::<72>(offset)?;
        ::core::result::Result::Ok(Self(buffer))
    }
}

impl<'a> ::planus::VectorRead<'a> for PlacementRef<'a> {
    const STRIDE: usize = 72;

    #[inline]
    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> Self {
        Self(unsafe { buffer.unchecked_advance_as_array(offset) })
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<Placement> for Placement {
    const STRIDE: usize = 72;

    type Value = Placement;

    #[inline]
    fn prepare(&self, _builder: &mut ::planus::Builder) -> Self::Value {
        *self
    }

    #[inline]
    unsafe fn write_values(
        values: &[Placement],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 72];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                
                buffer_position - (72 * i) as u32,
                
            );
        }
    }
}

    
/// The table `Cursor` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Table `Cursor` in the file `scene.fbs:57`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Cursor{
    
        /// The field `x` in the table `Cursor`
        pub x: u16,
        /// The field `y` in the table `Cursor`
        pub y: u16,
        /// The field `visible` in the table `Cursor`
        pub visible: bool,
        /// The field `blinking` in the table `Cursor`
        pub blinking: bool,
        /// The field `shape` in the table `Cursor`
        pub shape: self::CursorShape,
        /// The field `has_color` in the table `Cursor`
        pub has_color: bool,
        /// The field `color` in the table `Cursor`
        pub color: u32,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Cursor {
    fn default() -> Self {
        Self {
        x: 0,
        y: 0,
        visible: false,
        blinking: false,
        shape: self::CursorShape::Bar,
        has_color: false,
        color: 0,
        
        }
    }
}


impl Cursor {
    /// Creates a [CursorBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> CursorBuilder<()> {
        CursorBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_x: impl ::planus::WriteAsDefault<u16, u16>,
        field_y: impl ::planus::WriteAsDefault<u16, u16>,
        field_visible: impl ::planus::WriteAsDefault<bool, bool>,
        field_blinking: impl ::planus::WriteAsDefault<bool, bool>,
        field_shape: impl ::planus::WriteAsDefault<self::CursorShape, self::CursorShape>,
        field_has_color: impl ::planus::WriteAsDefault<bool, bool>,
        field_color: impl ::planus::WriteAsDefault<u32, u32>,
        
    ) -> ::planus::Offset<Self> {let prepared_x = field_x.prepare(builder, &0);let prepared_y = field_y.prepare(builder, &0);let prepared_visible = field_visible.prepare(builder, &false);let prepared_blinking = field_blinking.prepare(builder, &false);let prepared_shape = field_shape.prepare(builder, &self::CursorShape::Bar);let prepared_has_color = field_has_color.prepare(builder, &false);let prepared_color = field_color.prepare(builder, &0);

        let mut table_writer: ::planus::table_writer::TableWriter::<18> = ::core::default::Default::default();
        if prepared_color.is_some() {table_writer.write_entry::<u32>(6);}if prepared_x.is_some() {table_writer.write_entry::<u16>(0);}if prepared_y.is_some() {table_writer.write_entry::<u16>(1);}if prepared_visible.is_some() {table_writer.write_entry::<bool>(2);}if prepared_blinking.is_some() {table_writer.write_entry::<bool>(3);}if prepared_shape.is_some() {table_writer.write_entry::<self::CursorShape>(4);}if prepared_has_color.is_some() {table_writer.write_entry::<bool>(5);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_color) = prepared_color {object_writer.write::<_, _, 4>(&prepared_color);}if let ::core::option::Option::Some(prepared_x) = prepared_x {object_writer.write::<_, _, 2>(&prepared_x);}if let ::core::option::Option::Some(prepared_y) = prepared_y {object_writer.write::<_, _, 2>(&prepared_y);}if let ::core::option::Option::Some(prepared_visible) = prepared_visible {object_writer.write::<_, _, 1>(&prepared_visible);}if let ::core::option::Option::Some(prepared_blinking) = prepared_blinking {object_writer.write::<_, _, 1>(&prepared_blinking);}if let ::core::option::Option::Some(prepared_shape) = prepared_shape {object_writer.write::<_, _, 1>(&prepared_shape);}if let ::core::option::Option::Some(prepared_has_color) = prepared_has_color {object_writer.write::<_, _, 1>(&prepared_has_color);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Cursor>> for Cursor {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Cursor> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Cursor>> for Cursor {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Cursor>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Cursor> for Cursor {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Cursor> {
        Cursor::create(
            builder,
        
            
            self.x,
            
        
            
            self.y,
            
        
            
            self.visible,
            
        
            
            self.blinking,
            
        
            
            self.shape,
            
        
            
            self.has_color,
            
        
            
            self.color,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Cursor] type.
///
/// Can be created using the [Cursor::builder] method.
#[derive(Debug)]
#[must_use]
pub struct CursorBuilder<State>(State);

impl<
    
> CursorBuilder
<
    (
    
    )
>
{
    /// Setter for the [`x` field](Cursor#structfield.x).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn x<T0>(self, value: T0) -> CursorBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsDefault<u16, u16>
    {
        CursorBuilder((
            
            value,
        ))
    }

    
    /// Sets the [`x` field](Cursor#structfield.x) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn x_as_default(self) -> CursorBuilder<(
        
        ::planus::DefaultValue,
    )>
    {
        self.x(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
> CursorBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`y` field](Cursor#structfield.y).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn y<T1>(self, value: T1) -> CursorBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAsDefault<u16, u16>
    {
        
        let (
            
                v0,
            
        ) = self.0;CursorBuilder((
            
                v0,
            
            value,
        ))
    }

    
    /// Sets the [`y` field](Cursor#structfield.y) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn y_as_default(self) -> CursorBuilder<(
        
            T0,
        
        ::planus::DefaultValue,
    )>
    {
        self.y(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
> CursorBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`visible` field](Cursor#structfield.visible).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn visible<T2>(self, value: T2) -> CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;CursorBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    
    /// Sets the [`visible` field](Cursor#structfield.visible) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn visible_as_default(self) -> CursorBuilder<(
        
            T0,
        
            T1,
        
        ::planus::DefaultValue,
    )>
    {
        self.visible(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
> CursorBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
    )
>
{
    /// Setter for the [`blinking` field](Cursor#structfield.blinking).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn blinking<T3>(self, value: T3) -> CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )>
    where T3: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = self.0;CursorBuilder((
            
                v0,
            
                v1,
            
                v2,
            
            value,
        ))
    }

    
    /// Sets the [`blinking` field](Cursor#structfield.blinking) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn blinking_as_default(self) -> CursorBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
        ::planus::DefaultValue,
    )>
    {
        self.blinking(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
> CursorBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )
>
{
    /// Setter for the [`shape` field](Cursor#structfield.shape).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn shape<T4>(self, value: T4) -> CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )>
    where T4: ::planus::WriteAsDefault<self::CursorShape, self::CursorShape>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
        ) = self.0;CursorBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
            value,
        ))
    }

    
    /// Sets the [`shape` field](Cursor#structfield.shape) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn shape_as_default(self) -> CursorBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
        ::planus::DefaultValue,
    )>
    {
        self.shape(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
> CursorBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )
>
{
    /// Setter for the [`has_color` field](Cursor#structfield.has_color).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn has_color<T5>(self, value: T5) -> CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )>
    where T5: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        ) = self.0;CursorBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
            value,
        ))
    }

    
    /// Sets the [`has_color` field](Cursor#structfield.has_color) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn has_color_as_default(self) -> CursorBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
            T4,
        
        ::planus::DefaultValue,
    )>
    {
        self.has_color(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
> CursorBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )
>
{
    /// Setter for the [`color` field](Cursor#structfield.color).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn color<T6>(self, value: T6) -> CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
    )>
    where T6: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
        ) = self.0;CursorBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
            value,
        ))
    }

    
    /// Sets the [`color` field](Cursor#structfield.color) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn color_as_default(self) -> CursorBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
            T4,
        
            T5,
        
        ::planus::DefaultValue,
    )>
    {
        self.color(::planus::DefaultValue)
    }
    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
> CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Cursor].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Cursor>
        where Self: ::planus::WriteAsOffset<Cursor>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u16, u16>,
    
        T1: ::planus::WriteAsDefault<u16, u16>,
    
        T2: ::planus::WriteAsDefault<bool, bool>,
    
        T3: ::planus::WriteAsDefault<bool, bool>,
    
        T4: ::planus::WriteAsDefault<self::CursorShape, self::CursorShape>,
    
        T5: ::planus::WriteAsDefault<bool, bool>,
    
        T6: ::planus::WriteAsDefault<u32, u32>,
    
> ::planus::WriteAs<::planus::Offset<Cursor>> for CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
)> {
    type Prepared = ::planus::Offset<Cursor>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Cursor> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u16, u16>,
    
        T1: ::planus::WriteAsDefault<u16, u16>,
    
        T2: ::planus::WriteAsDefault<bool, bool>,
    
        T3: ::planus::WriteAsDefault<bool, bool>,
    
        T4: ::planus::WriteAsDefault<self::CursorShape, self::CursorShape>,
    
        T5: ::planus::WriteAsDefault<bool, bool>,
    
        T6: ::planus::WriteAsDefault<u32, u32>,
    
> ::planus::WriteAsOptional<::planus::Offset<Cursor>> for CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
)> {
    type Prepared = ::planus::Offset<Cursor>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Cursor>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u16, u16>,
    
        T1: ::planus::WriteAsDefault<u16, u16>,
    
        T2: ::planus::WriteAsDefault<bool, bool>,
    
        T3: ::planus::WriteAsDefault<bool, bool>,
    
        T4: ::planus::WriteAsDefault<self::CursorShape, self::CursorShape>,
    
        T5: ::planus::WriteAsDefault<bool, bool>,
    
        T6: ::planus::WriteAsDefault<u32, u32>,
    
> ::planus::WriteAsOffset<Cursor> for CursorBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Cursor> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
        ) = &self.0;Cursor::create(
            builder,
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
        )
    }
}

/// Reference to a deserialized [Cursor].
#[derive(Copy, Clone)]
pub struct CursorRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> CursorRef<'a> {
    
        /// Getter for the [`x` field](Cursor#structfield.x).
        #[inline]
        pub fn x(&self) -> ::planus::Result<u16> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(0, "Cursor", "x")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`y` field](Cursor#structfield.y).
        #[inline]
        pub fn y(&self) -> ::planus::Result<u16> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(1, "Cursor", "y")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`visible` field](Cursor#structfield.visible).
        #[inline]
        pub fn visible(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(2, "Cursor", "visible")
              
            
            ?.unwrap_or(false))
            
        }
    
        /// Getter for the [`blinking` field](Cursor#structfield.blinking).
        #[inline]
        pub fn blinking(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(3, "Cursor", "blinking")
              
            
            ?.unwrap_or(false))
            
        }
    
        /// Getter for the [`shape` field](Cursor#structfield.shape).
        #[inline]
        pub fn shape(&self) -> ::planus::Result<self::CursorShape> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(4, "Cursor", "shape")
              
            
            ?.unwrap_or(self::CursorShape::Bar))
            
        }
    
        /// Getter for the [`has_color` field](Cursor#structfield.has_color).
        #[inline]
        pub fn has_color(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(5, "Cursor", "has_color")
              
            
            ?.unwrap_or(false))
            
        }
    
        /// Getter for the [`color` field](Cursor#structfield.color).
        #[inline]
        pub fn color(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(6, "Cursor", "color")
              
            
            ?.unwrap_or(0))
            
        }
    
}

impl<'a> ::core::fmt::Debug for CursorRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("CursorRef");
        f.field("x", &self.x());f.field("y", &self.y());f.field("visible", &self.visible());f.field("blinking", &self.blinking());f.field("shape", &self.shape());f.field("has_color", &self.has_color());f.field("color", &self.color());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<CursorRef<'a>> for Cursor {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: CursorRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            x: ::core::convert::TryInto::try_into(value.x()?)?,y: ::core::convert::TryInto::try_into(value.y()?)?,visible: ::core::convert::TryInto::try_into(value.visible()?)?,blinking: ::core::convert::TryInto::try_into(value.blinking()?)?,shape: ::core::convert::TryInto::try_into(value.shape()?)?,has_color: ::core::convert::TryInto::try_into(value.has_color()?)?,color: ::core::convert::TryInto::try_into(value.color()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for CursorRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for CursorRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[CursorRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Cursor>> for Cursor {
    type Value = ::planus::Offset<Cursor>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Cursor>],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                buffer_position - (Self::STRIDE * i) as u32,
            );
        }
    }
}

impl<'a> ::planus::ReadAsRoot<'a> for CursorRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[CursorRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Image` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Table `Image` in the file `scene.fbs:67`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Image{
    
        /// The field `id` in the table `Image`
        pub id: u32,
        /// The field `generation` in the table `Image`
        pub generation: u64,
        /// The field `width` in the table `Image`
        pub width: u32,
        /// The field `height` in the table `Image`
        pub height: u32,
        /// The field `format` in the table `Image`
        pub format: self::PixelFormat,
        /// The field `pixels` in the table `Image`
        pub pixels: ::planus::alloc::vec::Vec<u8>,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Image {
    fn default() -> Self {
        Self {
        id: 0,
        generation: 0,
        width: 0,
        height: 0,
        format: self::PixelFormat::Rgb,
        pixels: ::core::default::Default::default(),
        
        }
    }
}


impl Image {
    /// Creates a [ImageBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> ImageBuilder<()> {
        ImageBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_id: impl ::planus::WriteAsDefault<u32, u32>,
        field_generation: impl ::planus::WriteAsDefault<u64, u64>,
        field_width: impl ::planus::WriteAsDefault<u32, u32>,
        field_height: impl ::planus::WriteAsDefault<u32, u32>,
        field_format: impl ::planus::WriteAsDefault<self::PixelFormat, self::PixelFormat>,
        field_pixels: impl ::planus::WriteAs<::planus::Offset<[u8]>>,
        
    ) -> ::planus::Offset<Self> {let prepared_id = field_id.prepare(builder, &0);let prepared_generation = field_generation.prepare(builder, &0);let prepared_width = field_width.prepare(builder, &0);let prepared_height = field_height.prepare(builder, &0);let prepared_format = field_format.prepare(builder, &self::PixelFormat::Rgb);let prepared_pixels = field_pixels.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<16> = ::core::default::Default::default();
        if prepared_generation.is_some() {table_writer.write_entry::<u64>(1);}if prepared_id.is_some() {table_writer.write_entry::<u32>(0);}if prepared_width.is_some() {table_writer.write_entry::<u32>(2);}if prepared_height.is_some() {table_writer.write_entry::<u32>(3);}table_writer.write_entry::<::planus::Offset<[u8]>>(5);if prepared_format.is_some() {table_writer.write_entry::<self::PixelFormat>(4);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_generation) = prepared_generation {object_writer.write::<_, _, 8>(&prepared_generation);}if let ::core::option::Option::Some(prepared_id) = prepared_id {object_writer.write::<_, _, 4>(&prepared_id);}if let ::core::option::Option::Some(prepared_width) = prepared_width {object_writer.write::<_, _, 4>(&prepared_width);}if let ::core::option::Option::Some(prepared_height) = prepared_height {object_writer.write::<_, _, 4>(&prepared_height);}object_writer.write::<_, _, 4>(&prepared_pixels);if let ::core::option::Option::Some(prepared_format) = prepared_format {object_writer.write::<_, _, 1>(&prepared_format);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Image>> for Image {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Image> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Image>> for Image {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Image>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Image> for Image {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Image> {
        Image::create(
            builder,
        
            
            self.id,
            
        
            
            self.generation,
            
        
            
            self.width,
            
        
            
            self.height,
            
        
            
            self.format,
            
        
            
            &self.pixels,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Image] type.
///
/// Can be created using the [Image::builder] method.
#[derive(Debug)]
#[must_use]
pub struct ImageBuilder<State>(State);

impl<
    
> ImageBuilder
<
    (
    
    )
>
{
    /// Setter for the [`id` field](Image#structfield.id).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn id<T0>(self, value: T0) -> ImageBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsDefault<u32, u32>
    {
        ImageBuilder((
            
            value,
        ))
    }

    
    /// Sets the [`id` field](Image#structfield.id) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn id_as_default(self) -> ImageBuilder<(
        
        ::planus::DefaultValue,
    )>
    {
        self.id(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
> ImageBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`generation` field](Image#structfield.generation).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn generation<T1>(self, value: T1) -> ImageBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAsDefault<u64, u64>
    {
        
        let (
            
                v0,
            
        ) = self.0;ImageBuilder((
            
                v0,
            
            value,
        ))
    }

    
    /// Sets the [`generation` field](Image#structfield.generation) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn generation_as_default(self) -> ImageBuilder<(
        
            T0,
        
        ::planus::DefaultValue,
    )>
    {
        self.generation(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
> ImageBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`width` field](Image#structfield.width).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn width<T2>(self, value: T2) -> ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;ImageBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    
    /// Sets the [`width` field](Image#structfield.width) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn width_as_default(self) -> ImageBuilder<(
        
            T0,
        
            T1,
        
        ::planus::DefaultValue,
    )>
    {
        self.width(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
> ImageBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
    )
>
{
    /// Setter for the [`height` field](Image#structfield.height).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn height<T3>(self, value: T3) -> ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )>
    where T3: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = self.0;ImageBuilder((
            
                v0,
            
                v1,
            
                v2,
            
            value,
        ))
    }

    
    /// Sets the [`height` field](Image#structfield.height) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn height_as_default(self) -> ImageBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
        ::planus::DefaultValue,
    )>
    {
        self.height(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
> ImageBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )
>
{
    /// Setter for the [`format` field](Image#structfield.format).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn format<T4>(self, value: T4) -> ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )>
    where T4: ::planus::WriteAsDefault<self::PixelFormat, self::PixelFormat>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
        ) = self.0;ImageBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
            value,
        ))
    }

    
    /// Sets the [`format` field](Image#structfield.format) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn format_as_default(self) -> ImageBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
        ::planus::DefaultValue,
    )>
    {
        self.format(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
> ImageBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )
>
{
    /// Setter for the [`pixels` field](Image#structfield.pixels).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn pixels<T5>(self, value: T5) -> ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )>
    where T5: ::planus::WriteAs<::planus::Offset<[u8]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        ) = self.0;ImageBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
> ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Image].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Image>
        where Self: ::planus::WriteAsOffset<Image>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u32, u32>,
    
        T1: ::planus::WriteAsDefault<u64, u64>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAsDefault<u32, u32>,
    
        T4: ::planus::WriteAsDefault<self::PixelFormat, self::PixelFormat>,
    
        T5: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
> ::planus::WriteAs<::planus::Offset<Image>> for ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    type Prepared = ::planus::Offset<Image>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Image> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u32, u32>,
    
        T1: ::planus::WriteAsDefault<u64, u64>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAsDefault<u32, u32>,
    
        T4: ::planus::WriteAsDefault<self::PixelFormat, self::PixelFormat>,
    
        T5: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
> ::planus::WriteAsOptional<::planus::Offset<Image>> for ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    type Prepared = ::planus::Offset<Image>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Image>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u32, u32>,
    
        T1: ::planus::WriteAsDefault<u64, u64>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAsDefault<u32, u32>,
    
        T4: ::planus::WriteAsDefault<self::PixelFormat, self::PixelFormat>,
    
        T5: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
> ::planus::WriteAsOffset<Image> for ImageBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Image> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
        ) = &self.0;Image::create(
            builder,
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
        )
    }
}

/// Reference to a deserialized [Image].
#[derive(Copy, Clone)]
pub struct ImageRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> ImageRef<'a> {
    
        /// Getter for the [`id` field](Image#structfield.id).
        #[inline]
        pub fn id(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(0, "Image", "id")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`generation` field](Image#structfield.generation).
        #[inline]
        pub fn generation(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(1, "Image", "generation")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`width` field](Image#structfield.width).
        #[inline]
        pub fn width(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(2, "Image", "width")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`height` field](Image#structfield.height).
        #[inline]
        pub fn height(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(3, "Image", "height")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`format` field](Image#structfield.format).
        #[inline]
        pub fn format(&self) -> ::planus::Result<self::PixelFormat> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(4, "Image", "format")
              
            
            ?.unwrap_or(self::PixelFormat::Rgb))
            
        }
    
        /// Getter for the [`pixels` field](Image#structfield.pixels).
        #[inline]
        pub fn pixels(&self) -> ::planus::Result<&'a [u8]> {
            
            
              
              self.0.access_required(5, "Image", "pixels")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for ImageRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("ImageRef");
        f.field("id", &self.id());f.field("generation", &self.generation());f.field("width", &self.width());f.field("height", &self.height());f.field("format", &self.format());f.field("pixels", &self.pixels());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<ImageRef<'a>> for Image {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: ImageRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            id: ::core::convert::TryInto::try_into(value.id()?)?,generation: ::core::convert::TryInto::try_into(value.generation()?)?,width: ::core::convert::TryInto::try_into(value.width()?)?,height: ::core::convert::TryInto::try_into(value.height()?)?,format: ::core::convert::TryInto::try_into(value.format()?)?,pixels: value.pixels()?.to_vec(),
        })
    }
}

impl<'a> ::planus::TableRead<'a> for ImageRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for ImageRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[ImageRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Image>> for Image {
    type Value = ::planus::Offset<Image>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Image>],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                buffer_position - (Self::STRIDE * i) as u32,
            );
        }
    }
}

impl<'a> ::planus::ReadAsRoot<'a> for ImageRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[ImageRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Frame` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Table `Frame` in the file `scene.fbs:76`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Frame{
    
        /// The field `size` in the table `Frame`
        pub size: self::Size,
        /// The field `cursor` in the table `Frame`
        pub cursor: ::planus::alloc::boxed::Box<self::Cursor>,
        /// The field `foreground` in the table `Frame`
        pub foreground: u32,
        /// The field `background` in the table `Frame`
        pub background: u32,
        /// The field `palette` in the table `Frame`
        pub palette: ::planus::alloc::vec::Vec<u32>,
        /// The field `images` in the table `Frame`
        pub images: ::planus::alloc::vec::Vec<self::Image>,
        /// The field `placements` in the table `Frame`
        pub placements: ::planus::alloc::vec::Vec<self::Placement>,
        /// The field `alternate_screen` in the table `Frame`
        pub alternate_screen: bool,
        /// The field `mouse_reporting` in the table `Frame`
        pub mouse_reporting: bool,
        /// The field `scrollback_total` in the table `Frame`
        pub scrollback_total: u64,
        /// The field `scrollback_offset` in the table `Frame`
        pub scrollback_offset: u64,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Frame {
    fn default() -> Self {
        Self {
        size: ::core::default::Default::default(),
        cursor: ::core::default::Default::default(),
        foreground: 0,
        background: 0,
        palette: ::core::default::Default::default(),
        images: ::core::default::Default::default(),
        placements: ::core::default::Default::default(),
        alternate_screen: false,
        mouse_reporting: false,
        scrollback_total: 0,
        scrollback_offset: 0,
        
        }
    }
}


impl Frame {
    /// Creates a [FrameBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> FrameBuilder<()> {
        FrameBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_size: impl ::planus::WriteAs<self::Size>,
        field_cursor: impl ::planus::WriteAs<::planus::Offset<self::Cursor>>,
        field_foreground: impl ::planus::WriteAsDefault<u32, u32>,
        field_background: impl ::planus::WriteAsDefault<u32, u32>,
        field_palette: impl ::planus::WriteAs<::planus::Offset<[u32]>>,
        field_images: impl ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Image>]>>,
        field_placements: impl ::planus::WriteAs<::planus::Offset<[self::Placement]>>,
        field_alternate_screen: impl ::planus::WriteAsDefault<bool, bool>,
        field_mouse_reporting: impl ::planus::WriteAsDefault<bool, bool>,
        field_scrollback_total: impl ::planus::WriteAsDefault<u64, u64>,
        field_scrollback_offset: impl ::planus::WriteAsDefault<u64, u64>,
        
    ) -> ::planus::Offset<Self> {let prepared_size = field_size.prepare(builder);let prepared_cursor = field_cursor.prepare(builder);let prepared_foreground = field_foreground.prepare(builder, &0);let prepared_background = field_background.prepare(builder, &0);let prepared_palette = field_palette.prepare(builder);let prepared_images = field_images.prepare(builder);let prepared_placements = field_placements.prepare(builder);let prepared_alternate_screen = field_alternate_screen.prepare(builder, &false);let prepared_mouse_reporting = field_mouse_reporting.prepare(builder, &false);let prepared_scrollback_total = field_scrollback_total.prepare(builder, &0);let prepared_scrollback_offset = field_scrollback_offset.prepare(builder, &0);

        let mut table_writer: ::planus::table_writer::TableWriter::<26> = ::core::default::Default::default();
        if prepared_scrollback_total.is_some() {table_writer.write_entry::<u64>(9);}if prepared_scrollback_offset.is_some() {table_writer.write_entry::<u64>(10);}table_writer.write_entry::<self::Size>(0);table_writer.write_entry::<::planus::Offset<self::Cursor>>(1);if prepared_foreground.is_some() {table_writer.write_entry::<u32>(2);}if prepared_background.is_some() {table_writer.write_entry::<u32>(3);}table_writer.write_entry::<::planus::Offset<[u32]>>(4);table_writer.write_entry::<::planus::Offset<[::planus::Offset<self::Image>]>>(5);table_writer.write_entry::<::planus::Offset<[self::Placement]>>(6);if prepared_alternate_screen.is_some() {table_writer.write_entry::<bool>(7);}if prepared_mouse_reporting.is_some() {table_writer.write_entry::<bool>(8);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_scrollback_total) = prepared_scrollback_total {object_writer.write::<_, _, 8>(&prepared_scrollback_total);}if let ::core::option::Option::Some(prepared_scrollback_offset) = prepared_scrollback_offset {object_writer.write::<_, _, 8>(&prepared_scrollback_offset);}object_writer.write::<_, _, 12>(&prepared_size);object_writer.write::<_, _, 4>(&prepared_cursor);if let ::core::option::Option::Some(prepared_foreground) = prepared_foreground {object_writer.write::<_, _, 4>(&prepared_foreground);}if let ::core::option::Option::Some(prepared_background) = prepared_background {object_writer.write::<_, _, 4>(&prepared_background);}object_writer.write::<_, _, 4>(&prepared_palette);object_writer.write::<_, _, 4>(&prepared_images);object_writer.write::<_, _, 4>(&prepared_placements);if let ::core::option::Option::Some(prepared_alternate_screen) = prepared_alternate_screen {object_writer.write::<_, _, 1>(&prepared_alternate_screen);}if let ::core::option::Option::Some(prepared_mouse_reporting) = prepared_mouse_reporting {object_writer.write::<_, _, 1>(&prepared_mouse_reporting);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Frame>> for Frame {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Frame> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Frame>> for Frame {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Frame>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Frame> for Frame {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Frame> {
        Frame::create(
            builder,
        
            
            self.size,
            
        
            
            &self.cursor,
            
        
            
            self.foreground,
            
        
            
            self.background,
            
        
            
            &self.palette,
            
        
            
            &self.images,
            
        
            
            &self.placements,
            
        
            
            self.alternate_screen,
            
        
            
            self.mouse_reporting,
            
        
            
            self.scrollback_total,
            
        
            
            self.scrollback_offset,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Frame] type.
///
/// Can be created using the [Frame::builder] method.
#[derive(Debug)]
#[must_use]
pub struct FrameBuilder<State>(State);

impl<
    
> FrameBuilder
<
    (
    
    )
>
{
    /// Setter for the [`size` field](Frame#structfield.size).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn size<T0>(self, value: T0) -> FrameBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAs<self::Size>
    {
        FrameBuilder((
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
> FrameBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`cursor` field](Frame#structfield.cursor).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn cursor<T1>(self, value: T1) -> FrameBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAs<::planus::Offset<self::Cursor>>
    {
        
        let (
            
                v0,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`foreground` field](Frame#structfield.foreground).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn foreground<T2>(self, value: T2) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    
    /// Sets the [`foreground` field](Frame#structfield.foreground) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn foreground_as_default(self) -> FrameBuilder<(
        
            T0,
        
            T1,
        
        ::planus::DefaultValue,
    )>
    {
        self.foreground(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
    )
>
{
    /// Setter for the [`background` field](Frame#structfield.background).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn background<T3>(self, value: T3) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )>
    where T3: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
            value,
        ))
    }

    
    /// Sets the [`background` field](Frame#structfield.background) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn background_as_default(self) -> FrameBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
        ::planus::DefaultValue,
    )>
    {
        self.background(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )
>
{
    /// Setter for the [`palette` field](Frame#structfield.palette).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn palette<T4>(self, value: T4) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )>
    where T4: ::planus::WriteAs<::planus::Offset<[u32]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )
>
{
    /// Setter for the [`images` field](Frame#structfield.images).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn images<T5>(self, value: T5) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )>
    where T5: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Image>]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )
>
{
    /// Setter for the [`placements` field](Frame#structfield.placements).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn placements<T6>(self, value: T6) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
    )>
    where T6: ::planus::WriteAs<::planus::Offset<[self::Placement]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
    )
>
{
    /// Setter for the [`alternate_screen` field](Frame#structfield.alternate_screen).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn alternate_screen<T7>(self, value: T7) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
    )>
    where T7: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
            value,
        ))
    }

    
    /// Sets the [`alternate_screen` field](Frame#structfield.alternate_screen) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn alternate_screen_as_default(self) -> FrameBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
            T4,
        
            T5,
        
            T6,
        
        ::planus::DefaultValue,
    )>
    {
        self.alternate_screen(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
    )
>
{
    /// Setter for the [`mouse_reporting` field](Frame#structfield.mouse_reporting).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn mouse_reporting<T8>(self, value: T8) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
    )>
    where T8: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
            value,
        ))
    }

    
    /// Sets the [`mouse_reporting` field](Frame#structfield.mouse_reporting) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn mouse_reporting_as_default(self) -> FrameBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
            T4,
        
            T5,
        
            T6,
        
            T7,
        
        ::planus::DefaultValue,
    )>
    {
        self.mouse_reporting(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
    )
>
{
    /// Setter for the [`scrollback_total` field](Frame#structfield.scrollback_total).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn scrollback_total<T9>(self, value: T9) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
    )>
    where T9: ::planus::WriteAsDefault<u64, u64>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
                v8,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
                v8,
            
            value,
        ))
    }

    
    /// Sets the [`scrollback_total` field](Frame#structfield.scrollback_total) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn scrollback_total_as_default(self) -> FrameBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
            T4,
        
            T5,
        
            T6,
        
            T7,
        
            T8,
        
        ::planus::DefaultValue,
    )>
    {
        self.scrollback_total(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
> FrameBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
    )
>
{
    /// Setter for the [`scrollback_offset` field](Frame#structfield.scrollback_offset).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn scrollback_offset<T10>(self, value: T10) -> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
        T10,
    
    )>
    where T10: ::planus::WriteAsDefault<u64, u64>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
                v8,
            
                v9,
            
        ) = self.0;FrameBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
                v8,
            
                v9,
            
            value,
        ))
    }

    
    /// Sets the [`scrollback_offset` field](Frame#structfield.scrollback_offset) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn scrollback_offset_as_default(self) -> FrameBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
            T4,
        
            T5,
        
            T6,
        
            T7,
        
            T8,
        
            T9,
        
        ::planus::DefaultValue,
    )>
    {
        self.scrollback_offset(::planus::DefaultValue)
    }
    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
        T10,
    
> FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
        T10,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Frame].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Frame>
        where Self: ::planus::WriteAsOffset<Frame>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<self::Size>,
    
        T1: ::planus::WriteAs<::planus::Offset<self::Cursor>>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAsDefault<u32, u32>,
    
        T4: ::planus::WriteAs<::planus::Offset<[u32]>>,
    
        T5: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Image>]>>,
    
        T6: ::planus::WriteAs<::planus::Offset<[self::Placement]>>,
    
        T7: ::planus::WriteAsDefault<bool, bool>,
    
        T8: ::planus::WriteAsDefault<bool, bool>,
    
        T9: ::planus::WriteAsDefault<u64, u64>,
    
        T10: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAs<::planus::Offset<Frame>> for FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
        T10,
    
)> {
    type Prepared = ::planus::Offset<Frame>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Frame> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<self::Size>,
    
        T1: ::planus::WriteAs<::planus::Offset<self::Cursor>>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAsDefault<u32, u32>,
    
        T4: ::planus::WriteAs<::planus::Offset<[u32]>>,
    
        T5: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Image>]>>,
    
        T6: ::planus::WriteAs<::planus::Offset<[self::Placement]>>,
    
        T7: ::planus::WriteAsDefault<bool, bool>,
    
        T8: ::planus::WriteAsDefault<bool, bool>,
    
        T9: ::planus::WriteAsDefault<u64, u64>,
    
        T10: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAsOptional<::planus::Offset<Frame>> for FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
        T10,
    
)> {
    type Prepared = ::planus::Offset<Frame>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Frame>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAs<self::Size>,
    
        T1: ::planus::WriteAs<::planus::Offset<self::Cursor>>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAsDefault<u32, u32>,
    
        T4: ::planus::WriteAs<::planus::Offset<[u32]>>,
    
        T5: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Image>]>>,
    
        T6: ::planus::WriteAs<::planus::Offset<[self::Placement]>>,
    
        T7: ::planus::WriteAsDefault<bool, bool>,
    
        T8: ::planus::WriteAsDefault<bool, bool>,
    
        T9: ::planus::WriteAsDefault<u64, u64>,
    
        T10: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAsOffset<Frame> for FrameBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
        T8,
    
        T9,
    
        T10,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Frame> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
                v8,
            
                v9,
            
                v10,
            
        ) = &self.0;Frame::create(
            builder,
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
                v8,
            
                v9,
            
                v10,
            
        )
    }
}

/// Reference to a deserialized [Frame].
#[derive(Copy, Clone)]
pub struct FrameRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> FrameRef<'a> {
    
        /// Getter for the [`size` field](Frame#structfield.size).
        #[inline]
        pub fn size(&self) -> ::planus::Result<self::SizeRef<'a>> {
            
            
              
              self.0.access_required(0, "Frame", "size")
              
            
            
            
        }
    
        /// Getter for the [`cursor` field](Frame#structfield.cursor).
        #[inline]
        pub fn cursor(&self) -> ::planus::Result<self::CursorRef<'a>> {
            
            
              
              self.0.access_required(1, "Frame", "cursor")
              
            
            
            
        }
    
        /// Getter for the [`foreground` field](Frame#structfield.foreground).
        #[inline]
        pub fn foreground(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(2, "Frame", "foreground")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`background` field](Frame#structfield.background).
        #[inline]
        pub fn background(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(3, "Frame", "background")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`palette` field](Frame#structfield.palette).
        #[inline]
        pub fn palette(&self) -> ::planus::Result<::planus::Vector<'a, u32>> {
            
            
              
              self.0.access_required(4, "Frame", "palette")
              
            
            
            
        }
    
        /// Getter for the [`images` field](Frame#structfield.images).
        #[inline]
        pub fn images(&self) -> ::planus::Result<::planus::Vector<'a, ::planus::Result<self::ImageRef<'a>>>> {
            
            
              
              self.0.access_required(5, "Frame", "images")
              
            
            
            
        }
    
        /// Getter for the [`placements` field](Frame#structfield.placements).
        #[inline]
        pub fn placements(&self) -> ::planus::Result<::planus::Vector<'a, self::PlacementRef<'a>>> {
            
            
              
              self.0.access_required(6, "Frame", "placements")
              
            
            
            
        }
    
        /// Getter for the [`alternate_screen` field](Frame#structfield.alternate_screen).
        #[inline]
        pub fn alternate_screen(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(7, "Frame", "alternate_screen")
              
            
            ?.unwrap_or(false))
            
        }
    
        /// Getter for the [`mouse_reporting` field](Frame#structfield.mouse_reporting).
        #[inline]
        pub fn mouse_reporting(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(8, "Frame", "mouse_reporting")
              
            
            ?.unwrap_or(false))
            
        }
    
        /// Getter for the [`scrollback_total` field](Frame#structfield.scrollback_total).
        #[inline]
        pub fn scrollback_total(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(9, "Frame", "scrollback_total")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`scrollback_offset` field](Frame#structfield.scrollback_offset).
        #[inline]
        pub fn scrollback_offset(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(10, "Frame", "scrollback_offset")
              
            
            ?.unwrap_or(0))
            
        }
    
}

impl<'a> ::core::fmt::Debug for FrameRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("FrameRef");
        f.field("size", &self.size());f.field("cursor", &self.cursor());f.field("foreground", &self.foreground());f.field("background", &self.background());f.field("palette", &self.palette());f.field("images", &self.images());f.field("placements", &self.placements());f.field("alternate_screen", &self.alternate_screen());f.field("mouse_reporting", &self.mouse_reporting());f.field("scrollback_total", &self.scrollback_total());f.field("scrollback_offset", &self.scrollback_offset());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<FrameRef<'a>> for Frame {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: FrameRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            size: ::core::convert::Into::into(value.size()?),cursor: ::planus::alloc::boxed::Box::new(::core::convert::TryInto::try_into(value.cursor()?)?),foreground: ::core::convert::TryInto::try_into(value.foreground()?)?,background: ::core::convert::TryInto::try_into(value.background()?)?,palette: value.palette()?.to_vec()?,images: value.images()?.to_vec_result()?,placements: value.placements()?.to_vec()?,alternate_screen: ::core::convert::TryInto::try_into(value.alternate_screen()?)?,mouse_reporting: ::core::convert::TryInto::try_into(value.mouse_reporting()?)?,scrollback_total: ::core::convert::TryInto::try_into(value.scrollback_total()?)?,scrollback_offset: ::core::convert::TryInto::try_into(value.scrollback_offset()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for FrameRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for FrameRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[FrameRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Frame>> for Frame {
    type Value = ::planus::Offset<Frame>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Frame>],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                buffer_position - (Self::STRIDE * i) as u32,
            );
        }
    }
}

impl<'a> ::planus::ReadAsRoot<'a> for FrameRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[FrameRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Run` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Table `Run` in the file `scene.fbs:90`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Run{
    
        /// The field `style` in the table `Run`
        pub style: u32,
        /// The field `has_link` in the table `Run`
        pub has_link: bool,
        /// The field `link` in the table `Run`
        pub link: u32,
        /// The field `text` in the table `Run`
        pub text: ::planus::alloc::string::String,
        /// The field `layout` in the table `Run`
        pub layout: ::planus::alloc::vec::Vec<self::Layout>,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Run {
    fn default() -> Self {
        Self {
        style: 0,
        has_link: false,
        link: 0,
        text: ::core::default::Default::default(),
        layout: ::core::default::Default::default(),
        
        }
    }
}


impl Run {
    /// Creates a [RunBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> RunBuilder<()> {
        RunBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_style: impl ::planus::WriteAsDefault<u32, u32>,
        field_has_link: impl ::planus::WriteAsDefault<bool, bool>,
        field_link: impl ::planus::WriteAsDefault<u32, u32>,
        field_text: impl ::planus::WriteAs<::planus::Offset<str>>,
        field_layout: impl ::planus::WriteAs<::planus::Offset<[self::Layout]>>,
        
    ) -> ::planus::Offset<Self> {let prepared_style = field_style.prepare(builder, &0);let prepared_has_link = field_has_link.prepare(builder, &false);let prepared_link = field_link.prepare(builder, &0);let prepared_text = field_text.prepare(builder);let prepared_layout = field_layout.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<14> = ::core::default::Default::default();
        if prepared_style.is_some() {table_writer.write_entry::<u32>(0);}if prepared_link.is_some() {table_writer.write_entry::<u32>(2);}table_writer.write_entry::<::planus::Offset<str>>(3);table_writer.write_entry::<::planus::Offset<[self::Layout]>>(4);if prepared_has_link.is_some() {table_writer.write_entry::<bool>(1);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_style) = prepared_style {object_writer.write::<_, _, 4>(&prepared_style);}if let ::core::option::Option::Some(prepared_link) = prepared_link {object_writer.write::<_, _, 4>(&prepared_link);}object_writer.write::<_, _, 4>(&prepared_text);object_writer.write::<_, _, 4>(&prepared_layout);if let ::core::option::Option::Some(prepared_has_link) = prepared_has_link {object_writer.write::<_, _, 1>(&prepared_has_link);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Run>> for Run {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Run> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Run>> for Run {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Run>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Run> for Run {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Run> {
        Run::create(
            builder,
        
            
            self.style,
            
        
            
            self.has_link,
            
        
            
            self.link,
            
        
            
            &self.text,
            
        
            
            &self.layout,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Run] type.
///
/// Can be created using the [Run::builder] method.
#[derive(Debug)]
#[must_use]
pub struct RunBuilder<State>(State);

impl<
    
> RunBuilder
<
    (
    
    )
>
{
    /// Setter for the [`style` field](Run#structfield.style).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn style<T0>(self, value: T0) -> RunBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsDefault<u32, u32>
    {
        RunBuilder((
            
            value,
        ))
    }

    
    /// Sets the [`style` field](Run#structfield.style) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn style_as_default(self) -> RunBuilder<(
        
        ::planus::DefaultValue,
    )>
    {
        self.style(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
> RunBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`has_link` field](Run#structfield.has_link).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn has_link<T1>(self, value: T1) -> RunBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
        ) = self.0;RunBuilder((
            
                v0,
            
            value,
        ))
    }

    
    /// Sets the [`has_link` field](Run#structfield.has_link) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn has_link_as_default(self) -> RunBuilder<(
        
            T0,
        
        ::planus::DefaultValue,
    )>
    {
        self.has_link(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
> RunBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`link` field](Run#structfield.link).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn link<T2>(self, value: T2) -> RunBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;RunBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    
    /// Sets the [`link` field](Run#structfield.link) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn link_as_default(self) -> RunBuilder<(
        
            T0,
        
            T1,
        
        ::planus::DefaultValue,
    )>
    {
        self.link(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
> RunBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
    )
>
{
    /// Setter for the [`text` field](Run#structfield.text).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn text<T3>(self, value: T3) -> RunBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )>
    where T3: ::planus::WriteAs<::planus::Offset<str>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = self.0;RunBuilder((
            
                v0,
            
                v1,
            
                v2,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
> RunBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )
>
{
    /// Setter for the [`layout` field](Run#structfield.layout).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn layout<T4>(self, value: T4) -> RunBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )>
    where T4: ::planus::WriteAs<::planus::Offset<[self::Layout]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
        ) = self.0;RunBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
> RunBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Run].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Run>
        where Self: ::planus::WriteAsOffset<Run>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u32, u32>,
    
        T1: ::planus::WriteAsDefault<bool, bool>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAs<::planus::Offset<str>>,
    
        T4: ::planus::WriteAs<::planus::Offset<[self::Layout]>>,
    
> ::planus::WriteAs<::planus::Offset<Run>> for RunBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    type Prepared = ::planus::Offset<Run>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Run> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u32, u32>,
    
        T1: ::planus::WriteAsDefault<bool, bool>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAs<::planus::Offset<str>>,
    
        T4: ::planus::WriteAs<::planus::Offset<[self::Layout]>>,
    
> ::planus::WriteAsOptional<::planus::Offset<Run>> for RunBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    type Prepared = ::planus::Offset<Run>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Run>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u32, u32>,
    
        T1: ::planus::WriteAsDefault<bool, bool>,
    
        T2: ::planus::WriteAsDefault<u32, u32>,
    
        T3: ::planus::WriteAs<::planus::Offset<str>>,
    
        T4: ::planus::WriteAs<::planus::Offset<[self::Layout]>>,
    
> ::planus::WriteAsOffset<Run> for RunBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Run> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        ) = &self.0;Run::create(
            builder,
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        )
    }
}

/// Reference to a deserialized [Run].
#[derive(Copy, Clone)]
pub struct RunRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> RunRef<'a> {
    
        /// Getter for the [`style` field](Run#structfield.style).
        #[inline]
        pub fn style(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(0, "Run", "style")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`has_link` field](Run#structfield.has_link).
        #[inline]
        pub fn has_link(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(1, "Run", "has_link")
              
            
            ?.unwrap_or(false))
            
        }
    
        /// Getter for the [`link` field](Run#structfield.link).
        #[inline]
        pub fn link(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(2, "Run", "link")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`text` field](Run#structfield.text).
        #[inline]
        pub fn text(&self) -> ::planus::Result<&'a ::core::primitive::str> {
            
            
              
              self.0.access_required(3, "Run", "text")
              
            
            
            
        }
    
        /// Getter for the [`layout` field](Run#structfield.layout).
        #[inline]
        pub fn layout(&self) -> ::planus::Result<::planus::Vector<'a, self::LayoutRef<'a>>> {
            
            
              
              self.0.access_required(4, "Run", "layout")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for RunRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("RunRef");
        f.field("style", &self.style());f.field("has_link", &self.has_link());f.field("link", &self.link());f.field("text", &self.text());f.field("layout", &self.layout());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<RunRef<'a>> for Run {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: RunRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            style: ::core::convert::TryInto::try_into(value.style()?)?,has_link: ::core::convert::TryInto::try_into(value.has_link()?)?,link: ::core::convert::TryInto::try_into(value.link()?)?,text: ::core::convert::Into::into(value.text()?),layout: value.layout()?.to_vec()?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for RunRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for RunRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[RunRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Run>> for Run {
    type Value = ::planus::Offset<Run>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Run>],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                buffer_position - (Self::STRIDE * i) as u32,
            );
        }
    }
}

impl<'a> ::planus::ReadAsRoot<'a> for RunRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[RunRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Row` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Table `Row` in the file `scene.fbs:98`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Row{
    
        /// The field `index` in the table `Row`
        pub index: u64,
        /// The field `runs` in the table `Row`
        pub runs: ::planus::alloc::vec::Vec<self::Run>,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Row {
    fn default() -> Self {
        Self {
        index: 0,
        runs: ::core::default::Default::default(),
        
        }
    }
}


impl Row {
    /// Creates a [RowBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> RowBuilder<()> {
        RowBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_index: impl ::planus::WriteAsDefault<u64, u64>,
        field_runs: impl ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Run>]>>,
        
    ) -> ::planus::Offset<Self> {let prepared_index = field_index.prepare(builder, &0);let prepared_runs = field_runs.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<8> = ::core::default::Default::default();
        if prepared_index.is_some() {table_writer.write_entry::<u64>(0);}table_writer.write_entry::<::planus::Offset<[::planus::Offset<self::Run>]>>(1);

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_index) = prepared_index {object_writer.write::<_, _, 8>(&prepared_index);}object_writer.write::<_, _, 4>(&prepared_runs);
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Row>> for Row {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Row> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Row>> for Row {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Row>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Row> for Row {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Row> {
        Row::create(
            builder,
        
            
            self.index,
            
        
            
            &self.runs,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Row] type.
///
/// Can be created using the [Row::builder] method.
#[derive(Debug)]
#[must_use]
pub struct RowBuilder<State>(State);

impl<
    
> RowBuilder
<
    (
    
    )
>
{
    /// Setter for the [`index` field](Row#structfield.index).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn index<T0>(self, value: T0) -> RowBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsDefault<u64, u64>
    {
        RowBuilder((
            
            value,
        ))
    }

    
    /// Sets the [`index` field](Row#structfield.index) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn index_as_default(self) -> RowBuilder<(
        
        ::planus::DefaultValue,
    )>
    {
        self.index(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
> RowBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`runs` field](Row#structfield.runs).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn runs<T1>(self, value: T1) -> RowBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Run>]>>
    {
        
        let (
            
                v0,
            
        ) = self.0;RowBuilder((
            
                v0,
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
        T1,
    
> RowBuilder<(
    
        T0,
    
        T1,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Row].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Row>
        where Self: ::planus::WriteAsOffset<Row>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
        T1: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Run>]>>,
    
> ::planus::WriteAs<::planus::Offset<Row>> for RowBuilder<(
    
        T0,
    
        T1,
    
)> {
    type Prepared = ::planus::Offset<Row>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Row> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
        T1: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Run>]>>,
    
> ::planus::WriteAsOptional<::planus::Offset<Row>> for RowBuilder<(
    
        T0,
    
        T1,
    
)> {
    type Prepared = ::planus::Offset<Row>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Row>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
        T1: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Run>]>>,
    
> ::planus::WriteAsOffset<Row> for RowBuilder<(
    
        T0,
    
        T1,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Row> {
        
        let (
            
                v0,
            
                v1,
            
        ) = &self.0;Row::create(
            builder,
            
                v0,
            
                v1,
            
        )
    }
}

/// Reference to a deserialized [Row].
#[derive(Copy, Clone)]
pub struct RowRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> RowRef<'a> {
    
        /// Getter for the [`index` field](Row#structfield.index).
        #[inline]
        pub fn index(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(0, "Row", "index")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`runs` field](Row#structfield.runs).
        #[inline]
        pub fn runs(&self) -> ::planus::Result<::planus::Vector<'a, ::planus::Result<self::RunRef<'a>>>> {
            
            
              
              self.0.access_required(1, "Row", "runs")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for RowRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("RowRef");
        f.field("index", &self.index());f.field("runs", &self.runs());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<RowRef<'a>> for Row {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: RowRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            index: ::core::convert::TryInto::try_into(value.index()?)?,runs: value.runs()?.to_vec_result()?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for RowRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for RowRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[RowRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Row>> for Row {
    type Value = ::planus::Offset<Row>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Row>],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                buffer_position - (Self::STRIDE * i) as u32,
            );
        }
    }
}

impl<'a> ::planus::ReadAsRoot<'a> for RowRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[RowRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Scene` in the namespace `scene`
/// 
/// Generated from these locations:
/// * Table `Scene` in the file `scene.fbs:103`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Scene{
    
        /// The field `version` in the table `Scene`
        pub version: u8,
        /// The field `revision` in the table `Scene`
        pub revision: u64,
        /// The field `has_base_revision` in the table `Scene`
        pub has_base_revision: bool,
        /// The field `base_revision` in the table `Scene`
        pub base_revision: u64,
        /// The field `frame` in the table `Scene`
        pub frame: ::planus::alloc::boxed::Box<self::Frame>,
        /// The field `styles` in the table `Scene`
        pub styles: ::planus::alloc::vec::Vec<self::Style>,
        /// The field `hyperlinks` in the table `Scene`
        pub hyperlinks: ::planus::alloc::vec::Vec<::planus::alloc::string::String>,
        /// The field `rows` in the table `Scene`
        pub rows: ::planus::alloc::vec::Vec<self::Row>,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Scene {
    fn default() -> Self {
        Self {
        version: 0,
        revision: 0,
        has_base_revision: false,
        base_revision: 0,
        frame: ::core::default::Default::default(),
        styles: ::core::default::Default::default(),
        hyperlinks: ::core::default::Default::default(),
        rows: ::core::default::Default::default(),
        
        }
    }
}


impl Scene {
    /// Creates a [SceneBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> SceneBuilder<()> {
        SceneBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_version: impl ::planus::WriteAsDefault<u8, u8>,
        field_revision: impl ::planus::WriteAsDefault<u64, u64>,
        field_has_base_revision: impl ::planus::WriteAsDefault<bool, bool>,
        field_base_revision: impl ::planus::WriteAsDefault<u64, u64>,
        field_frame: impl ::planus::WriteAs<::planus::Offset<self::Frame>>,
        field_styles: impl ::planus::WriteAs<::planus::Offset<[self::Style]>>,
        field_hyperlinks: impl ::planus::WriteAs<::planus::Offset<[::planus::Offset<str>]>>,
        field_rows: impl ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Row>]>>,
        
    ) -> ::planus::Offset<Self> {let prepared_version = field_version.prepare(builder, &0);let prepared_revision = field_revision.prepare(builder, &0);let prepared_has_base_revision = field_has_base_revision.prepare(builder, &false);let prepared_base_revision = field_base_revision.prepare(builder, &0);let prepared_frame = field_frame.prepare(builder);let prepared_styles = field_styles.prepare(builder);let prepared_hyperlinks = field_hyperlinks.prepare(builder);let prepared_rows = field_rows.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<20> = ::core::default::Default::default();
        if prepared_revision.is_some() {table_writer.write_entry::<u64>(1);}if prepared_base_revision.is_some() {table_writer.write_entry::<u64>(3);}table_writer.write_entry::<::planus::Offset<self::Frame>>(4);table_writer.write_entry::<::planus::Offset<[self::Style]>>(5);table_writer.write_entry::<::planus::Offset<[::planus::Offset<str>]>>(6);table_writer.write_entry::<::planus::Offset<[::planus::Offset<self::Row>]>>(7);if prepared_version.is_some() {table_writer.write_entry::<u8>(0);}if prepared_has_base_revision.is_some() {table_writer.write_entry::<bool>(2);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_revision) = prepared_revision {object_writer.write::<_, _, 8>(&prepared_revision);}if let ::core::option::Option::Some(prepared_base_revision) = prepared_base_revision {object_writer.write::<_, _, 8>(&prepared_base_revision);}object_writer.write::<_, _, 4>(&prepared_frame);object_writer.write::<_, _, 4>(&prepared_styles);object_writer.write::<_, _, 4>(&prepared_hyperlinks);object_writer.write::<_, _, 4>(&prepared_rows);if let ::core::option::Option::Some(prepared_version) = prepared_version {object_writer.write::<_, _, 1>(&prepared_version);}if let ::core::option::Option::Some(prepared_has_base_revision) = prepared_has_base_revision {object_writer.write::<_, _, 1>(&prepared_has_base_revision);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Scene>> for Scene {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Scene> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Scene>> for Scene {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Scene>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Scene> for Scene {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Scene> {
        Scene::create(
            builder,
        
            
            self.version,
            
        
            
            self.revision,
            
        
            
            self.has_base_revision,
            
        
            
            self.base_revision,
            
        
            
            &self.frame,
            
        
            
            &self.styles,
            
        
            
            &self.hyperlinks,
            
        
            
            &self.rows,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Scene] type.
///
/// Can be created using the [Scene::builder] method.
#[derive(Debug)]
#[must_use]
pub struct SceneBuilder<State>(State);

impl<
    
> SceneBuilder
<
    (
    
    )
>
{
    /// Setter for the [`version` field](Scene#structfield.version).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn version<T0>(self, value: T0) -> SceneBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsDefault<u8, u8>
    {
        SceneBuilder((
            
            value,
        ))
    }

    
    /// Sets the [`version` field](Scene#structfield.version) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn version_as_default(self) -> SceneBuilder<(
        
        ::planus::DefaultValue,
    )>
    {
        self.version(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
> SceneBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`revision` field](Scene#structfield.revision).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn revision<T1>(self, value: T1) -> SceneBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAsDefault<u64, u64>
    {
        
        let (
            
                v0,
            
        ) = self.0;SceneBuilder((
            
                v0,
            
            value,
        ))
    }

    
    /// Sets the [`revision` field](Scene#structfield.revision) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn revision_as_default(self) -> SceneBuilder<(
        
            T0,
        
        ::planus::DefaultValue,
    )>
    {
        self.revision(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
> SceneBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`has_base_revision` field](Scene#structfield.has_base_revision).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn has_base_revision<T2>(self, value: T2) -> SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;SceneBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    
    /// Sets the [`has_base_revision` field](Scene#structfield.has_base_revision) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn has_base_revision_as_default(self) -> SceneBuilder<(
        
            T0,
        
            T1,
        
        ::planus::DefaultValue,
    )>
    {
        self.has_base_revision(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
> SceneBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
    )
>
{
    /// Setter for the [`base_revision` field](Scene#structfield.base_revision).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn base_revision<T3>(self, value: T3) -> SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )>
    where T3: ::planus::WriteAsDefault<u64, u64>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = self.0;SceneBuilder((
            
                v0,
            
                v1,
            
                v2,
            
            value,
        ))
    }

    
    /// Sets the [`base_revision` field](Scene#structfield.base_revision) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn base_revision_as_default(self) -> SceneBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
        ::planus::DefaultValue,
    )>
    {
        self.base_revision(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
> SceneBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )
>
{
    /// Setter for the [`frame` field](Scene#structfield.frame).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn frame<T4>(self, value: T4) -> SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )>
    where T4: ::planus::WriteAs<::planus::Offset<self::Frame>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
        ) = self.0;SceneBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
> SceneBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )
>
{
    /// Setter for the [`styles` field](Scene#structfield.styles).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn styles<T5>(self, value: T5) -> SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )>
    where T5: ::planus::WriteAs<::planus::Offset<[self::Style]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        ) = self.0;SceneBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
> SceneBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )
>
{
    /// Setter for the [`hyperlinks` field](Scene#structfield.hyperlinks).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn hyperlinks<T6>(self, value: T6) -> SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
    )>
    where T6: ::planus::WriteAs<::planus::Offset<[::planus::Offset<str>]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
        ) = self.0;SceneBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
> SceneBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
    )
>
{
    /// Setter for the [`rows` field](Scene#structfield.rows).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn rows<T7>(self, value: T7) -> SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
    )>
    where T7: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Row>]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
        ) = self.0;SceneBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
> SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Scene].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Scene>
        where Self: ::planus::WriteAsOffset<Scene>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u8, u8>,
    
        T1: ::planus::WriteAsDefault<u64, u64>,
    
        T2: ::planus::WriteAsDefault<bool, bool>,
    
        T3: ::planus::WriteAsDefault<u64, u64>,
    
        T4: ::planus::WriteAs<::planus::Offset<self::Frame>>,
    
        T5: ::planus::WriteAs<::planus::Offset<[self::Style]>>,
    
        T6: ::planus::WriteAs<::planus::Offset<[::planus::Offset<str>]>>,
    
        T7: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Row>]>>,
    
> ::planus::WriteAs<::planus::Offset<Scene>> for SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
)> {
    type Prepared = ::planus::Offset<Scene>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Scene> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u8, u8>,
    
        T1: ::planus::WriteAsDefault<u64, u64>,
    
        T2: ::planus::WriteAsDefault<bool, bool>,
    
        T3: ::planus::WriteAsDefault<u64, u64>,
    
        T4: ::planus::WriteAs<::planus::Offset<self::Frame>>,
    
        T5: ::planus::WriteAs<::planus::Offset<[self::Style]>>,
    
        T6: ::planus::WriteAs<::planus::Offset<[::planus::Offset<str>]>>,
    
        T7: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Row>]>>,
    
> ::planus::WriteAsOptional<::planus::Offset<Scene>> for SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
)> {
    type Prepared = ::planus::Offset<Scene>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Scene>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u8, u8>,
    
        T1: ::planus::WriteAsDefault<u64, u64>,
    
        T2: ::planus::WriteAsDefault<bool, bool>,
    
        T3: ::planus::WriteAsDefault<u64, u64>,
    
        T4: ::planus::WriteAs<::planus::Offset<self::Frame>>,
    
        T5: ::planus::WriteAs<::planus::Offset<[self::Style]>>,
    
        T6: ::planus::WriteAs<::planus::Offset<[::planus::Offset<str>]>>,
    
        T7: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::Row>]>>,
    
> ::planus::WriteAsOffset<Scene> for SceneBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
        T6,
    
        T7,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Scene> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
        ) = &self.0;Scene::create(
            builder,
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
                v6,
            
                v7,
            
        )
    }
}

/// Reference to a deserialized [Scene].
#[derive(Copy, Clone)]
pub struct SceneRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> SceneRef<'a> {
    
        /// Getter for the [`version` field](Scene#structfield.version).
        #[inline]
        pub fn version(&self) -> ::planus::Result<u8> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(0, "Scene", "version")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`revision` field](Scene#structfield.revision).
        #[inline]
        pub fn revision(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(1, "Scene", "revision")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`has_base_revision` field](Scene#structfield.has_base_revision).
        #[inline]
        pub fn has_base_revision(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(2, "Scene", "has_base_revision")
              
            
            ?.unwrap_or(false))
            
        }
    
        /// Getter for the [`base_revision` field](Scene#structfield.base_revision).
        #[inline]
        pub fn base_revision(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(3, "Scene", "base_revision")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`frame` field](Scene#structfield.frame).
        #[inline]
        pub fn frame(&self) -> ::planus::Result<self::FrameRef<'a>> {
            
            
              
              self.0.access_required(4, "Scene", "frame")
              
            
            
            
        }
    
        /// Getter for the [`styles` field](Scene#structfield.styles).
        #[inline]
        pub fn styles(&self) -> ::planus::Result<::planus::Vector<'a, self::StyleRef<'a>>> {
            
            
              
              self.0.access_required(5, "Scene", "styles")
              
            
            
            
        }
    
        /// Getter for the [`hyperlinks` field](Scene#structfield.hyperlinks).
        #[inline]
        pub fn hyperlinks(&self) -> ::planus::Result<::planus::Vector<'a, ::planus::Result<&'a ::core::primitive::str>>> {
            
            
              
              self.0.access_required(6, "Scene", "hyperlinks")
              
            
            
            
        }
    
        /// Getter for the [`rows` field](Scene#structfield.rows).
        #[inline]
        pub fn rows(&self) -> ::planus::Result<::planus::Vector<'a, ::planus::Result<self::RowRef<'a>>>> {
            
            
              
              self.0.access_required(7, "Scene", "rows")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for SceneRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("SceneRef");
        f.field("version", &self.version());f.field("revision", &self.revision());f.field("has_base_revision", &self.has_base_revision());f.field("base_revision", &self.base_revision());f.field("frame", &self.frame());f.field("styles", &self.styles());f.field("hyperlinks", &self.hyperlinks());f.field("rows", &self.rows());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<SceneRef<'a>> for Scene {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: SceneRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            version: ::core::convert::TryInto::try_into(value.version()?)?,revision: ::core::convert::TryInto::try_into(value.revision()?)?,has_base_revision: ::core::convert::TryInto::try_into(value.has_base_revision()?)?,base_revision: ::core::convert::TryInto::try_into(value.base_revision()?)?,frame: ::planus::alloc::boxed::Box::new(::core::convert::TryInto::try_into(value.frame()?)?),styles: value.styles()?.to_vec()?,hyperlinks: value.hyperlinks()?.to_vec_result()?,rows: value.rows()?.to_vec_result()?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for SceneRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for SceneRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[SceneRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Scene>> for Scene {
    type Value = ::planus::Offset<Scene>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Scene>],
        bytes: *mut ::core::mem::MaybeUninit<u8>,
        buffer_position: u32,
    ) {
        let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
        for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
            ::planus::WriteAsPrimitive::write(
                v,
                ::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
                buffer_position - (Self::STRIDE * i) as u32,
            );
        }
    }
}

impl<'a> ::planus::ReadAsRoot<'a> for SceneRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[SceneRef]",
            "read_as_root",
            0,
        ))
    }
}

    }}