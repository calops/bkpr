pub use root::*;

const _: () = ::planus::check_version_compatibility("planus-1.3.0");


/// The root namespace
/// 
/// Generated from these locations:
/// * File `crates/bkpr-protocol/src/remote_wire/remote.fbs`
#[no_implicit_prelude]
#[allow(clippy::needless_lifetimes)]
mod root {
/// The namespace `remote_wire`
/// 
/// Generated from these locations:
/// * File `crates/bkpr-protocol/src/remote_wire/remote.fbs`
pub mod remote_wire {
/// The table `Hello` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `Hello` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:5`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Hello{
    
        /// The field `session_name` in the table `Hello`
        pub session_name: ::planus::alloc::string::String,
        /// The field `native_family` in the table `Hello`
        pub native_family: u32,
        /// The field `required_capabilities` in the table `Hello`
        pub required_capabilities: u64,
        /// The field `connection_nonce` in the table `Hello`
        pub connection_nonce: ::planus::alloc::vec::Vec<u8>,
        /// The field `forward_agent` in the table `Hello`
        pub forward_agent: bool,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Hello {
    fn default() -> Self {
        Self {
        session_name: ::core::default::Default::default(),
        native_family: 0,
        required_capabilities: 0,
        connection_nonce: ::core::default::Default::default(),
        forward_agent: false,
        
        }
    }
}


impl Hello {
    /// Creates a [HelloBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> HelloBuilder<()> {
        HelloBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_session_name: impl ::planus::WriteAs<::planus::Offset<str>>,
        field_native_family: impl ::planus::WriteAsDefault<u32, u32>,
        field_required_capabilities: impl ::planus::WriteAsDefault<u64, u64>,
        field_connection_nonce: impl ::planus::WriteAs<::planus::Offset<[u8]>>,
        field_forward_agent: impl ::planus::WriteAsDefault<bool, bool>,
        
    ) -> ::planus::Offset<Self> {let prepared_session_name = field_session_name.prepare(builder);let prepared_native_family = field_native_family.prepare(builder, &0);let prepared_required_capabilities = field_required_capabilities.prepare(builder, &0);let prepared_connection_nonce = field_connection_nonce.prepare(builder);let prepared_forward_agent = field_forward_agent.prepare(builder, &false);

        let mut table_writer: ::planus::table_writer::TableWriter::<14> = ::core::default::Default::default();
        if prepared_required_capabilities.is_some() {table_writer.write_entry::<u64>(2);}table_writer.write_entry::<::planus::Offset<str>>(0);if prepared_native_family.is_some() {table_writer.write_entry::<u32>(1);}table_writer.write_entry::<::planus::Offset<[u8]>>(3);if prepared_forward_agent.is_some() {table_writer.write_entry::<bool>(4);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_required_capabilities) = prepared_required_capabilities {object_writer.write::<_, _, 8>(&prepared_required_capabilities);}object_writer.write::<_, _, 4>(&prepared_session_name);if let ::core::option::Option::Some(prepared_native_family) = prepared_native_family {object_writer.write::<_, _, 4>(&prepared_native_family);}object_writer.write::<_, _, 4>(&prepared_connection_nonce);if let ::core::option::Option::Some(prepared_forward_agent) = prepared_forward_agent {object_writer.write::<_, _, 1>(&prepared_forward_agent);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Hello>> for Hello {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Hello> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Hello>> for Hello {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Hello>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Hello> for Hello {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Hello> {
        Hello::create(
            builder,
        
            
            &self.session_name,
            
        
            
            self.native_family,
            
        
            
            self.required_capabilities,
            
        
            
            &self.connection_nonce,
            
        
            
            self.forward_agent,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Hello] type.
///
/// Can be created using the [Hello::builder] method.
#[derive(Debug)]
#[must_use]
pub struct HelloBuilder<State>(State);

impl<
    
> HelloBuilder
<
    (
    
    )
>
{
    /// Setter for the [`session_name` field](Hello#structfield.session_name).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn session_name<T0>(self, value: T0) -> HelloBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAs<::planus::Offset<str>>
    {
        HelloBuilder((
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
> HelloBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`native_family` field](Hello#structfield.native_family).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn native_family<T1>(self, value: T1) -> HelloBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
        ) = self.0;HelloBuilder((
            
                v0,
            
            value,
        ))
    }

    
    /// Sets the [`native_family` field](Hello#structfield.native_family) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn native_family_as_default(self) -> HelloBuilder<(
        
            T0,
        
        ::planus::DefaultValue,
    )>
    {
        self.native_family(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
> HelloBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`required_capabilities` field](Hello#structfield.required_capabilities).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn required_capabilities<T2>(self, value: T2) -> HelloBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAsDefault<u64, u64>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;HelloBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    
    /// Sets the [`required_capabilities` field](Hello#structfield.required_capabilities) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn required_capabilities_as_default(self) -> HelloBuilder<(
        
            T0,
        
            T1,
        
        ::planus::DefaultValue,
    )>
    {
        self.required_capabilities(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
> HelloBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
    )
>
{
    /// Setter for the [`connection_nonce` field](Hello#structfield.connection_nonce).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn connection_nonce<T3>(self, value: T3) -> HelloBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )>
    where T3: ::planus::WriteAs<::planus::Offset<[u8]>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = self.0;HelloBuilder((
            
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
    
> HelloBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )
>
{
    /// Setter for the [`forward_agent` field](Hello#structfield.forward_agent).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn forward_agent<T4>(self, value: T4) -> HelloBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )>
    where T4: ::planus::WriteAsDefault<bool, bool>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
        ) = self.0;HelloBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
            value,
        ))
    }

    
    /// Sets the [`forward_agent` field](Hello#structfield.forward_agent) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn forward_agent_as_default(self) -> HelloBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
        ::planus::DefaultValue,
    )>
    {
        self.forward_agent(::planus::DefaultValue)
    }
    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
> HelloBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Hello].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Hello>
        where Self: ::planus::WriteAsOffset<Hello>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
        T1: ::planus::WriteAsDefault<u32, u32>,
    
        T2: ::planus::WriteAsDefault<u64, u64>,
    
        T3: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
        T4: ::planus::WriteAsDefault<bool, bool>,
    
> ::planus::WriteAs<::planus::Offset<Hello>> for HelloBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    type Prepared = ::planus::Offset<Hello>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Hello> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
        T1: ::planus::WriteAsDefault<u32, u32>,
    
        T2: ::planus::WriteAsDefault<u64, u64>,
    
        T3: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
        T4: ::planus::WriteAsDefault<bool, bool>,
    
> ::planus::WriteAsOptional<::planus::Offset<Hello>> for HelloBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    type Prepared = ::planus::Offset<Hello>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Hello>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
        T1: ::planus::WriteAsDefault<u32, u32>,
    
        T2: ::planus::WriteAsDefault<u64, u64>,
    
        T3: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
        T4: ::planus::WriteAsDefault<bool, bool>,
    
> ::planus::WriteAsOffset<Hello> for HelloBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Hello> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        ) = &self.0;Hello::create(
            builder,
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        )
    }
}

/// Reference to a deserialized [Hello].
#[derive(Copy, Clone)]
pub struct HelloRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> HelloRef<'a> {
    
        /// Getter for the [`session_name` field](Hello#structfield.session_name).
        #[inline]
        pub fn session_name(&self) -> ::planus::Result<&'a ::core::primitive::str> {
            
            
              
              self.0.access_required(0, "Hello", "session_name")
              
            
            
            
        }
    
        /// Getter for the [`native_family` field](Hello#structfield.native_family).
        #[inline]
        pub fn native_family(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(1, "Hello", "native_family")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`required_capabilities` field](Hello#structfield.required_capabilities).
        #[inline]
        pub fn required_capabilities(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(2, "Hello", "required_capabilities")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`connection_nonce` field](Hello#structfield.connection_nonce).
        #[inline]
        pub fn connection_nonce(&self) -> ::planus::Result<&'a [u8]> {
            
            
              
              self.0.access_required(3, "Hello", "connection_nonce")
              
            
            
            
        }
    
        /// Getter for the [`forward_agent` field](Hello#structfield.forward_agent).
        #[inline]
        pub fn forward_agent(&self) -> ::planus::Result<bool> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(4, "Hello", "forward_agent")
              
            
            ?.unwrap_or(false))
            
        }
    
}

impl<'a> ::core::fmt::Debug for HelloRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("HelloRef");
        f.field("session_name", &self.session_name());f.field("native_family", &self.native_family());f.field("required_capabilities", &self.required_capabilities());f.field("connection_nonce", &self.connection_nonce());f.field("forward_agent", &self.forward_agent());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<HelloRef<'a>> for Hello {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: HelloRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            session_name: ::core::convert::Into::into(value.session_name()?),native_family: ::core::convert::TryInto::try_into(value.native_family()?)?,required_capabilities: ::core::convert::TryInto::try_into(value.required_capabilities()?)?,connection_nonce: value.connection_nonce()?.to_vec(),forward_agent: ::core::convert::TryInto::try_into(value.forward_agent()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for HelloRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for HelloRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[HelloRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Hello>> for Hello {
    type Value = ::planus::Offset<Hello>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Hello>],
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

impl<'a> ::planus::ReadAsRoot<'a> for HelloRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[HelloRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `SessionInfo` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `SessionInfo` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:12`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct SessionInfo{
    
        /// The field `name` in the table `SessionInfo`
        pub name: ::planus::alloc::string::String,
        /// The field `session_id` in the table `SessionInfo`
        pub session_id: ::planus::alloc::string::String,
        /// The field `daemon_id` in the table `SessionInfo`
        pub daemon_id: ::planus::alloc::string::String,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for SessionInfo {
    fn default() -> Self {
        Self {
        name: ::core::default::Default::default(),
        session_id: ::core::default::Default::default(),
        daemon_id: ::core::default::Default::default(),
        
        }
    }
}


impl SessionInfo {
    /// Creates a [SessionInfoBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> SessionInfoBuilder<()> {
        SessionInfoBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_name: impl ::planus::WriteAs<::planus::Offset<str>>,
        field_session_id: impl ::planus::WriteAs<::planus::Offset<str>>,
        field_daemon_id: impl ::planus::WriteAs<::planus::Offset<str>>,
        
    ) -> ::planus::Offset<Self> {let prepared_name = field_name.prepare(builder);let prepared_session_id = field_session_id.prepare(builder);let prepared_daemon_id = field_daemon_id.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<10> = ::core::default::Default::default();
        table_writer.write_entry::<::planus::Offset<str>>(0);table_writer.write_entry::<::planus::Offset<str>>(1);table_writer.write_entry::<::planus::Offset<str>>(2);

        unsafe {
            table_writer.finish(builder, |object_writer| {object_writer.write::<_, _, 4>(&prepared_name);object_writer.write::<_, _, 4>(&prepared_session_id);object_writer.write::<_, _, 4>(&prepared_daemon_id);
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<SessionInfo>> for SessionInfo {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionInfo> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<SessionInfo>> for SessionInfo {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<SessionInfo>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<SessionInfo> for SessionInfo {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionInfo> {
        SessionInfo::create(
            builder,
        
            
            &self.name,
            
        
            
            &self.session_id,
            
        
            
            &self.daemon_id,
            
        
        )
    }
}

/// Builder for serializing an instance of the [SessionInfo] type.
///
/// Can be created using the [SessionInfo::builder] method.
#[derive(Debug)]
#[must_use]
pub struct SessionInfoBuilder<State>(State);

impl<
    
> SessionInfoBuilder
<
    (
    
    )
>
{
    /// Setter for the [`name` field](SessionInfo#structfield.name).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn name<T0>(self, value: T0) -> SessionInfoBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAs<::planus::Offset<str>>
    {
        SessionInfoBuilder((
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
> SessionInfoBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`session_id` field](SessionInfo#structfield.session_id).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn session_id<T1>(self, value: T1) -> SessionInfoBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAs<::planus::Offset<str>>
    {
        
        let (
            
                v0,
            
        ) = self.0;SessionInfoBuilder((
            
                v0,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
> SessionInfoBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`daemon_id` field](SessionInfo#structfield.daemon_id).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn daemon_id<T2>(self, value: T2) -> SessionInfoBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAs<::planus::Offset<str>>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;SessionInfoBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
> SessionInfoBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [SessionInfo].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionInfo>
        where Self: ::planus::WriteAsOffset<SessionInfo>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
        T1: ::planus::WriteAs<::planus::Offset<str>>,
    
        T2: ::planus::WriteAs<::planus::Offset<str>>,
    
> ::planus::WriteAs<::planus::Offset<SessionInfo>> for SessionInfoBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
)> {
    type Prepared = ::planus::Offset<SessionInfo>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionInfo> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
        T1: ::planus::WriteAs<::planus::Offset<str>>,
    
        T2: ::planus::WriteAs<::planus::Offset<str>>,
    
> ::planus::WriteAsOptional<::planus::Offset<SessionInfo>> for SessionInfoBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
)> {
    type Prepared = ::planus::Offset<SessionInfo>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<SessionInfo>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
        T1: ::planus::WriteAs<::planus::Offset<str>>,
    
        T2: ::planus::WriteAs<::planus::Offset<str>>,
    
> ::planus::WriteAsOffset<SessionInfo> for SessionInfoBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionInfo> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = &self.0;SessionInfo::create(
            builder,
            
                v0,
            
                v1,
            
                v2,
            
        )
    }
}

/// Reference to a deserialized [SessionInfo].
#[derive(Copy, Clone)]
pub struct SessionInfoRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> SessionInfoRef<'a> {
    
        /// Getter for the [`name` field](SessionInfo#structfield.name).
        #[inline]
        pub fn name(&self) -> ::planus::Result<&'a ::core::primitive::str> {
            
            
              
              self.0.access_required(0, "SessionInfo", "name")
              
            
            
            
        }
    
        /// Getter for the [`session_id` field](SessionInfo#structfield.session_id).
        #[inline]
        pub fn session_id(&self) -> ::planus::Result<&'a ::core::primitive::str> {
            
            
              
              self.0.access_required(1, "SessionInfo", "session_id")
              
            
            
            
        }
    
        /// Getter for the [`daemon_id` field](SessionInfo#structfield.daemon_id).
        #[inline]
        pub fn daemon_id(&self) -> ::planus::Result<&'a ::core::primitive::str> {
            
            
              
              self.0.access_required(2, "SessionInfo", "daemon_id")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for SessionInfoRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("SessionInfoRef");
        f.field("name", &self.name());f.field("session_id", &self.session_id());f.field("daemon_id", &self.daemon_id());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<SessionInfoRef<'a>> for SessionInfo {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: SessionInfoRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            name: ::core::convert::Into::into(value.name()?),session_id: ::core::convert::Into::into(value.session_id()?),daemon_id: ::core::convert::Into::into(value.daemon_id()?),
        })
    }
}

impl<'a> ::planus::TableRead<'a> for SessionInfoRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for SessionInfoRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[SessionInfoRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<SessionInfo>> for SessionInfo {
    type Value = ::planus::Offset<SessionInfo>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<SessionInfo>],
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

impl<'a> ::planus::ReadAsRoot<'a> for SessionInfoRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[SessionInfoRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `UnixPath` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `UnixPath` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:17`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct UnixPath{
    
        /// The field `bytes` in the table `UnixPath`
        pub bytes: ::planus::alloc::vec::Vec<u8>,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for UnixPath {
    fn default() -> Self {
        Self {
        bytes: ::core::default::Default::default(),
        
        }
    }
}


impl UnixPath {
    /// Creates a [UnixPathBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> UnixPathBuilder<()> {
        UnixPathBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_bytes: impl ::planus::WriteAs<::planus::Offset<[u8]>>,
        
    ) -> ::planus::Offset<Self> {let prepared_bytes = field_bytes.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<6> = ::core::default::Default::default();
        table_writer.write_entry::<::planus::Offset<[u8]>>(0);

        unsafe {
            table_writer.finish(builder, |object_writer| {object_writer.write::<_, _, 4>(&prepared_bytes);
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<UnixPath>> for UnixPath {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<UnixPath> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<UnixPath>> for UnixPath {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<UnixPath>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<UnixPath> for UnixPath {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<UnixPath> {
        UnixPath::create(
            builder,
        
            
            &self.bytes,
            
        
        )
    }
}

/// Builder for serializing an instance of the [UnixPath] type.
///
/// Can be created using the [UnixPath::builder] method.
#[derive(Debug)]
#[must_use]
pub struct UnixPathBuilder<State>(State);

impl<
    
> UnixPathBuilder
<
    (
    
    )
>
{
    /// Setter for the [`bytes` field](UnixPath#structfield.bytes).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn bytes<T0>(self, value: T0) -> UnixPathBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAs<::planus::Offset<[u8]>>
    {
        UnixPathBuilder((
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
> UnixPathBuilder<(
    
        T0,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [UnixPath].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<UnixPath>
        where Self: ::planus::WriteAsOffset<UnixPath>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
> ::planus::WriteAs<::planus::Offset<UnixPath>> for UnixPathBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<UnixPath>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<UnixPath> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
> ::planus::WriteAsOptional<::planus::Offset<UnixPath>> for UnixPathBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<UnixPath>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<UnixPath>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
> ::planus::WriteAsOffset<UnixPath> for UnixPathBuilder<(
    
        T0,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<UnixPath> {
        
        let (
            
                v0,
            
        ) = &self.0;UnixPath::create(
            builder,
            
                v0,
            
        )
    }
}

/// Reference to a deserialized [UnixPath].
#[derive(Copy, Clone)]
pub struct UnixPathRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> UnixPathRef<'a> {
    
        /// Getter for the [`bytes` field](UnixPath#structfield.bytes).
        #[inline]
        pub fn bytes(&self) -> ::planus::Result<&'a [u8]> {
            
            
              
              self.0.access_required(0, "UnixPath", "bytes")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for UnixPathRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("UnixPathRef");
        f.field("bytes", &self.bytes());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<UnixPathRef<'a>> for UnixPath {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: UnixPathRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            bytes: value.bytes()?.to_vec(),
        })
    }
}

impl<'a> ::planus::TableRead<'a> for UnixPathRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for UnixPathRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[UnixPathRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<UnixPath>> for UnixPath {
    type Value = ::planus::Offset<UnixPath>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<UnixPath>],
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

impl<'a> ::planus::ReadAsRoot<'a> for UnixPathRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[UnixPathRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The union `AgentPath` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Union `AgentPath` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:18`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize)]
pub enum AgentPath{
    
        /// The variant of type `UnixPath` in the union `AgentPath`
        UnixPath(::planus::alloc::boxed::Box<self::UnixPath>),
    
}


impl AgentPath {
    /// Creates a [AgentPathBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> AgentPathBuilder<::planus::Uninitialized> {
        AgentPathBuilder(::planus::Uninitialized)
    }

    #[inline]
    pub fn create_unix_path(
      builder: &mut ::planus::Builder,
      value: impl ::planus::WriteAsOffset<self::UnixPath>,
    ) -> ::planus::UnionOffset<Self> {
        ::planus::UnionOffset::new(1, value.prepare(builder).downcast())
    }

    
}



impl ::planus::WriteAsUnion<AgentPath> for AgentPath {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Self> {
        match self {
            Self::UnixPath(value) => Self::create_unix_path(builder, value),
            
        }
    }
}


impl ::planus::WriteAsOptionalUnion<AgentPath> for AgentPath {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Self>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}

/// Builder for serializing an instance of the [AgentPath] type.
///
/// Can be created using the [AgentPath::builder] method.
#[derive(Debug)]
#[must_use]
pub struct AgentPathBuilder<T>(T);

impl AgentPathBuilder<::planus::Uninitialized> {
    /// Creates an instance of the [`UnixPath` variant](AgentPath#variant.UnixPath).
    #[inline]
    pub fn unix_path<T>(
        self,
        value: T,
    ) -> AgentPathBuilder<::planus::Initialized<1, T>>
        where T: ::planus::WriteAsOffset<self::UnixPath> {
        AgentPathBuilder(::planus::Initialized(value))
    }

    
}

impl<const N: u8, T> AgentPathBuilder<::planus::Initialized<N, T>> {
    /// Finish writing the builder to get an [UnionOffset](::planus::UnionOffset) to a serialized [AgentPath].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<AgentPath>
        where Self: ::planus::WriteAsUnion<AgentPath>
    {
        ::planus::WriteAsUnion::prepare(&self, builder)
    }
}

impl<T> ::planus::WriteAsUnion<AgentPath> for AgentPathBuilder<::planus::Initialized<1, T>>
    where T: ::planus::WriteAsOffset<self::UnixPath>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<AgentPath> {
        ::planus::UnionOffset::new(1, (self.0).0.prepare(builder).downcast())
    }
}

impl<T> ::planus::WriteAsOptionalUnion<AgentPath> for AgentPathBuilder<::planus::Initialized<1, T>>
    where T: ::planus::WriteAsOffset<self::UnixPath>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<AgentPath>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}


/// Reference to a deserialized [AgentPath].
#[derive(Copy, Clone, Debug)]
pub enum AgentPathRef<'a>{
    UnixPath(self::UnixPathRef<'a>),
    
}


impl<'a> ::core::convert::TryFrom<AgentPathRef<'a>> for AgentPath {
    type Error = ::planus::Error;

    fn try_from(value: AgentPathRef<'a>) -> ::planus::Result<Self> {
        ::core::result::Result::Ok(match value {
            
                AgentPathRef::UnixPath(value) => Self::UnixPath(::planus::alloc::boxed::Box::new(::core::convert::TryFrom::try_from(value)?)),
                
            
        })
    }
}



impl<'a> ::planus::TableReadUnion<'a> for AgentPathRef<'a> {
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, tag: u8, field_offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        match tag {
            1 => ::core::result::Result::Ok(Self::UnixPath(::planus::TableRead::from_buffer(buffer, field_offset)?)),_ => ::core::result::Result::Err(::planus::errors::ErrorKind::UnknownUnionTag { tag }),
        }
    }
}



impl<'a> ::planus::VectorReadUnion<'a> for AgentPathRef<'a> {
    const VECTOR_NAME: &'static str = "[AgentPathRef]";
}


    
/// The table `OptionalAgentPath` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `OptionalAgentPath` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:20`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct OptionalAgentPath{
    
        /// The field `value` in the table `OptionalAgentPath`
        pub value: self::AgentPath,}



impl OptionalAgentPath {
    /// Creates a [OptionalAgentPathBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> OptionalAgentPathBuilder<()> {
        OptionalAgentPathBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_value: impl ::planus::WriteAsUnion<self::AgentPath>,
        
    ) -> ::planus::Offset<Self> {let prepared_value = field_value.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<8> = ::core::default::Default::default();
        table_writer.write_entry::<::planus::Offset<self::AgentPath>>(1);table_writer.write_entry::<u8>(0);

        unsafe {
            table_writer.finish(builder, |object_writer| {object_writer.write::<_, _, 4>(&prepared_value.offset());object_writer.write::<_, _, 1>(&prepared_value.tag());
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<OptionalAgentPath>> for OptionalAgentPath {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<OptionalAgentPath> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<OptionalAgentPath>> for OptionalAgentPath {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<OptionalAgentPath>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<OptionalAgentPath> for OptionalAgentPath {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<OptionalAgentPath> {
        OptionalAgentPath::create(
            builder,
        
            
            &self.value,
            
        
        )
    }
}

/// Builder for serializing an instance of the [OptionalAgentPath] type.
///
/// Can be created using the [OptionalAgentPath::builder] method.
#[derive(Debug)]
#[must_use]
pub struct OptionalAgentPathBuilder<State>(State);

impl<
    
> OptionalAgentPathBuilder
<
    (
    
    )
>
{
    /// Setter for the [`value` field](OptionalAgentPath#structfield.value).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn value<T0>(self, value: T0) -> OptionalAgentPathBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsUnion<self::AgentPath>
    {
        OptionalAgentPathBuilder((
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
> OptionalAgentPathBuilder<(
    
        T0,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [OptionalAgentPath].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<OptionalAgentPath>
        where Self: ::planus::WriteAsOffset<OptionalAgentPath>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsUnion<self::AgentPath>,
    
> ::planus::WriteAs<::planus::Offset<OptionalAgentPath>> for OptionalAgentPathBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<OptionalAgentPath>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<OptionalAgentPath> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsUnion<self::AgentPath>,
    
> ::planus::WriteAsOptional<::planus::Offset<OptionalAgentPath>> for OptionalAgentPathBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<OptionalAgentPath>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<OptionalAgentPath>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsUnion<self::AgentPath>,
    
> ::planus::WriteAsOffset<OptionalAgentPath> for OptionalAgentPathBuilder<(
    
        T0,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<OptionalAgentPath> {
        
        let (
            
                v0,
            
        ) = &self.0;OptionalAgentPath::create(
            builder,
            
                v0,
            
        )
    }
}

/// Reference to a deserialized [OptionalAgentPath].
#[derive(Copy, Clone)]
pub struct OptionalAgentPathRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> OptionalAgentPathRef<'a> {
    
        /// Getter for the [`value` field](OptionalAgentPath#structfield.value).
        #[inline]
        pub fn value(&self) -> ::planus::Result<self::AgentPathRef<'a>> {
            
            
              
              self.0.access_union_required(0, "OptionalAgentPath", "value")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for OptionalAgentPathRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("OptionalAgentPathRef");
        f.field("value", &self.value());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<OptionalAgentPathRef<'a>> for OptionalAgentPath {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: OptionalAgentPathRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            value: ::core::convert::TryInto::try_into(value.value()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for OptionalAgentPathRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for OptionalAgentPathRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[OptionalAgentPathRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<OptionalAgentPath>> for OptionalAgentPath {
    type Value = ::planus::Offset<OptionalAgentPath>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<OptionalAgentPath>],
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

impl<'a> ::planus::ReadAsRoot<'a> for OptionalAgentPathRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[OptionalAgentPathRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Welcome` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `Welcome` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:21`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Welcome{
    
        /// The field `session` in the table `Welcome`
        pub session: ::planus::alloc::boxed::Box<self::SessionInfo>,
        /// The field `connection_nonce` in the table `Welcome`
        pub connection_nonce: ::planus::alloc::vec::Vec<u8>,
        /// The field `capabilities` in the table `Welcome`
        pub capabilities: u64,
        /// The field `agent_socket` in the table `Welcome`
        pub agent_socket: ::core::option::Option<::planus::alloc::boxed::Box<self::OptionalAgentPath>>,
        /// The field `heartbeat_interval_ms` in the table `Welcome`
        pub heartbeat_interval_ms: u32,
        /// The field `lease_timeout_ms` in the table `Welcome`
        pub lease_timeout_ms: u32,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Welcome {
    fn default() -> Self {
        Self {
        session: ::core::default::Default::default(),
        connection_nonce: ::core::default::Default::default(),
        capabilities: 0,
        agent_socket: ::core::default::Default::default(),
        heartbeat_interval_ms: 0,
        lease_timeout_ms: 0,
        
        }
    }
}


impl Welcome {
    /// Creates a [WelcomeBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> WelcomeBuilder<()> {
        WelcomeBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_session: impl ::planus::WriteAs<::planus::Offset<self::SessionInfo>>,
        field_connection_nonce: impl ::planus::WriteAs<::planus::Offset<[u8]>>,
        field_capabilities: impl ::planus::WriteAsDefault<u64, u64>,
        field_agent_socket: impl ::planus::WriteAsOptional<::planus::Offset<self::OptionalAgentPath>>,
        field_heartbeat_interval_ms: impl ::planus::WriteAsDefault<u32, u32>,
        field_lease_timeout_ms: impl ::planus::WriteAsDefault<u32, u32>,
        
    ) -> ::planus::Offset<Self> {let prepared_session = field_session.prepare(builder);let prepared_connection_nonce = field_connection_nonce.prepare(builder);let prepared_capabilities = field_capabilities.prepare(builder, &0);let prepared_agent_socket = field_agent_socket.prepare(builder);let prepared_heartbeat_interval_ms = field_heartbeat_interval_ms.prepare(builder, &0);let prepared_lease_timeout_ms = field_lease_timeout_ms.prepare(builder, &0);

        let mut table_writer: ::planus::table_writer::TableWriter::<16> = ::core::default::Default::default();
        if prepared_capabilities.is_some() {table_writer.write_entry::<u64>(2);}table_writer.write_entry::<::planus::Offset<self::SessionInfo>>(0);table_writer.write_entry::<::planus::Offset<[u8]>>(1);if prepared_agent_socket.is_some() {table_writer.write_entry::<::planus::Offset<self::OptionalAgentPath>>(3);}if prepared_heartbeat_interval_ms.is_some() {table_writer.write_entry::<u32>(4);}if prepared_lease_timeout_ms.is_some() {table_writer.write_entry::<u32>(5);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_capabilities) = prepared_capabilities {object_writer.write::<_, _, 8>(&prepared_capabilities);}object_writer.write::<_, _, 4>(&prepared_session);object_writer.write::<_, _, 4>(&prepared_connection_nonce);if let ::core::option::Option::Some(prepared_agent_socket) = prepared_agent_socket {object_writer.write::<_, _, 4>(&prepared_agent_socket);}if let ::core::option::Option::Some(prepared_heartbeat_interval_ms) = prepared_heartbeat_interval_ms {object_writer.write::<_, _, 4>(&prepared_heartbeat_interval_ms);}if let ::core::option::Option::Some(prepared_lease_timeout_ms) = prepared_lease_timeout_ms {object_writer.write::<_, _, 4>(&prepared_lease_timeout_ms);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Welcome>> for Welcome {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Welcome> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Welcome>> for Welcome {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Welcome>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Welcome> for Welcome {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Welcome> {
        Welcome::create(
            builder,
        
            
            &self.session,
            
        
            
            &self.connection_nonce,
            
        
            
            self.capabilities,
            
        
            
            &self.agent_socket,
            
        
            
            self.heartbeat_interval_ms,
            
        
            
            self.lease_timeout_ms,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Welcome] type.
///
/// Can be created using the [Welcome::builder] method.
#[derive(Debug)]
#[must_use]
pub struct WelcomeBuilder<State>(State);

impl<
    
> WelcomeBuilder
<
    (
    
    )
>
{
    /// Setter for the [`session` field](Welcome#structfield.session).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn session<T0>(self, value: T0) -> WelcomeBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAs<::planus::Offset<self::SessionInfo>>
    {
        WelcomeBuilder((
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
> WelcomeBuilder
<
    (
    
        T0,
    
    )
>
{
    /// Setter for the [`connection_nonce` field](Welcome#structfield.connection_nonce).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn connection_nonce<T1>(self, value: T1) -> WelcomeBuilder<(
    
        T0,
    
        T1,
    
    )>
    where T1: ::planus::WriteAs<::planus::Offset<[u8]>>
    {
        
        let (
            
                v0,
            
        ) = self.0;WelcomeBuilder((
            
                v0,
            
            value,
        ))
    }

    

    
}

impl<
    
        T0,
    
        T1,
    
> WelcomeBuilder
<
    (
    
        T0,
    
        T1,
    
    )
>
{
    /// Setter for the [`capabilities` field](Welcome#structfield.capabilities).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn capabilities<T2>(self, value: T2) -> WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
    )>
    where T2: ::planus::WriteAsDefault<u64, u64>
    {
        
        let (
            
                v0,
            
                v1,
            
        ) = self.0;WelcomeBuilder((
            
                v0,
            
                v1,
            
            value,
        ))
    }

    
    /// Sets the [`capabilities` field](Welcome#structfield.capabilities) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn capabilities_as_default(self) -> WelcomeBuilder<(
        
            T0,
        
            T1,
        
        ::planus::DefaultValue,
    )>
    {
        self.capabilities(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
> WelcomeBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
    )
>
{
    /// Setter for the [`agent_socket` field](Welcome#structfield.agent_socket).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn agent_socket<T3>(self, value: T3) -> WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )>
    where T3: ::planus::WriteAsOptional<::planus::Offset<self::OptionalAgentPath>>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
        ) = self.0;WelcomeBuilder((
            
                v0,
            
                v1,
            
                v2,
            
            value,
        ))
    }

    

    
    /// Sets the [`agent_socket` field](Welcome#structfield.agent_socket) to null.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn agent_socket_as_null(self) -> WelcomeBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
        (),
    )>
    {
        self.agent_socket(())
    }
    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
> WelcomeBuilder
<
    (
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
    )
>
{
    /// Setter for the [`heartbeat_interval_ms` field](Welcome#structfield.heartbeat_interval_ms).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn heartbeat_interval_ms<T4>(self, value: T4) -> WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
    )>
    where T4: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
        ) = self.0;WelcomeBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
            value,
        ))
    }

    
    /// Sets the [`heartbeat_interval_ms` field](Welcome#structfield.heartbeat_interval_ms) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn heartbeat_interval_ms_as_default(self) -> WelcomeBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
        ::planus::DefaultValue,
    )>
    {
        self.heartbeat_interval_ms(::planus::DefaultValue)
    }
    

    
}

impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
> WelcomeBuilder
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
    /// Setter for the [`lease_timeout_ms` field](Welcome#structfield.lease_timeout_ms).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn lease_timeout_ms<T5>(self, value: T5) -> WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
    )>
    where T5: ::planus::WriteAsDefault<u32, u32>
    {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
        ) = self.0;WelcomeBuilder((
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
            value,
        ))
    }

    
    /// Sets the [`lease_timeout_ms` field](Welcome#structfield.lease_timeout_ms) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn lease_timeout_ms_as_default(self) -> WelcomeBuilder<(
        
            T0,
        
            T1,
        
            T2,
        
            T3,
        
            T4,
        
        ::planus::DefaultValue,
    )>
    {
        self.lease_timeout_ms(::planus::DefaultValue)
    }
    

    
}



impl<
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
> WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Welcome].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Welcome>
        where Self: ::planus::WriteAsOffset<Welcome>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<self::SessionInfo>>,
    
        T1: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
        T2: ::planus::WriteAsDefault<u64, u64>,
    
        T3: ::planus::WriteAsOptional<::planus::Offset<self::OptionalAgentPath>>,
    
        T4: ::planus::WriteAsDefault<u32, u32>,
    
        T5: ::planus::WriteAsDefault<u32, u32>,
    
> ::planus::WriteAs<::planus::Offset<Welcome>> for WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    type Prepared = ::planus::Offset<Welcome>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Welcome> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<self::SessionInfo>>,
    
        T1: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
        T2: ::planus::WriteAsDefault<u64, u64>,
    
        T3: ::planus::WriteAsOptional<::planus::Offset<self::OptionalAgentPath>>,
    
        T4: ::planus::WriteAsDefault<u32, u32>,
    
        T5: ::planus::WriteAsDefault<u32, u32>,
    
> ::planus::WriteAsOptional<::planus::Offset<Welcome>> for WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    type Prepared = ::planus::Offset<Welcome>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Welcome>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<self::SessionInfo>>,
    
        T1: ::planus::WriteAs<::planus::Offset<[u8]>>,
    
        T2: ::planus::WriteAsDefault<u64, u64>,
    
        T3: ::planus::WriteAsOptional<::planus::Offset<self::OptionalAgentPath>>,
    
        T4: ::planus::WriteAsDefault<u32, u32>,
    
        T5: ::planus::WriteAsDefault<u32, u32>,
    
> ::planus::WriteAsOffset<Welcome> for WelcomeBuilder<(
    
        T0,
    
        T1,
    
        T2,
    
        T3,
    
        T4,
    
        T5,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Welcome> {
        
        let (
            
                v0,
            
                v1,
            
                v2,
            
                v3,
            
                v4,
            
                v5,
            
        ) = &self.0;Welcome::create(
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

/// Reference to a deserialized [Welcome].
#[derive(Copy, Clone)]
pub struct WelcomeRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> WelcomeRef<'a> {
    
        /// Getter for the [`session` field](Welcome#structfield.session).
        #[inline]
        pub fn session(&self) -> ::planus::Result<self::SessionInfoRef<'a>> {
            
            
              
              self.0.access_required(0, "Welcome", "session")
              
            
            
            
        }
    
        /// Getter for the [`connection_nonce` field](Welcome#structfield.connection_nonce).
        #[inline]
        pub fn connection_nonce(&self) -> ::planus::Result<&'a [u8]> {
            
            
              
              self.0.access_required(1, "Welcome", "connection_nonce")
              
            
            
            
        }
    
        /// Getter for the [`capabilities` field](Welcome#structfield.capabilities).
        #[inline]
        pub fn capabilities(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(2, "Welcome", "capabilities")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`agent_socket` field](Welcome#structfield.agent_socket).
        #[inline]
        pub fn agent_socket(&self) -> ::planus::Result<::core::option::Option<self::OptionalAgentPathRef<'a>>> {
            
            
              
              self.0.access(3, "Welcome", "agent_socket")
              
            
            
            
        }
    
        /// Getter for the [`heartbeat_interval_ms` field](Welcome#structfield.heartbeat_interval_ms).
        #[inline]
        pub fn heartbeat_interval_ms(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(4, "Welcome", "heartbeat_interval_ms")
              
            
            ?.unwrap_or(0))
            
        }
    
        /// Getter for the [`lease_timeout_ms` field](Welcome#structfield.lease_timeout_ms).
        #[inline]
        pub fn lease_timeout_ms(&self) -> ::planus::Result<u32> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(5, "Welcome", "lease_timeout_ms")
              
            
            ?.unwrap_or(0))
            
        }
    
}

impl<'a> ::core::fmt::Debug for WelcomeRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("WelcomeRef");
        f.field("session", &self.session());f.field("connection_nonce", &self.connection_nonce());f.field("capabilities", &self.capabilities());if let ::core::option::Option::Some(field_agent_socket) = self.agent_socket().transpose() {
                f.field("agent_socket", &field_agent_socket);
            }f.field("heartbeat_interval_ms", &self.heartbeat_interval_ms());f.field("lease_timeout_ms", &self.lease_timeout_ms());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<WelcomeRef<'a>> for Welcome {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: WelcomeRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            session: ::planus::alloc::boxed::Box::new(::core::convert::TryInto::try_into(value.session()?)?),connection_nonce: value.connection_nonce()?.to_vec(),capabilities: ::core::convert::TryInto::try_into(value.capabilities()?)?,agent_socket: 
                                if let ::core::option::Option::Some(agent_socket) = value.agent_socket()? {
                                    ::core::option::Option::Some(::planus::alloc::boxed::Box::new(::core::convert::TryInto::try_into(agent_socket)?))
                                } else {
                                    ::core::option::Option::None
                                }
                            ,heartbeat_interval_ms: ::core::convert::TryInto::try_into(value.heartbeat_interval_ms()?)?,lease_timeout_ms: ::core::convert::TryInto::try_into(value.lease_timeout_ms()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for WelcomeRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for WelcomeRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[WelcomeRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Welcome>> for Welcome {
    type Value = ::planus::Offset<Welcome>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Welcome>],
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

impl<'a> ::planus::ReadAsRoot<'a> for WelcomeRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[WelcomeRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Failure` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `Failure` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:29`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Failure{
    
        /// The field `message` in the table `Failure`
        pub message: ::planus::alloc::string::String,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Failure {
    fn default() -> Self {
        Self {
        message: ::core::default::Default::default(),
        
        }
    }
}


impl Failure {
    /// Creates a [FailureBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> FailureBuilder<()> {
        FailureBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_message: impl ::planus::WriteAs<::planus::Offset<str>>,
        
    ) -> ::planus::Offset<Self> {let prepared_message = field_message.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<6> = ::core::default::Default::default();
        table_writer.write_entry::<::planus::Offset<str>>(0);

        unsafe {
            table_writer.finish(builder, |object_writer| {object_writer.write::<_, _, 4>(&prepared_message);
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Failure>> for Failure {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Failure> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Failure>> for Failure {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Failure>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Failure> for Failure {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Failure> {
        Failure::create(
            builder,
        
            
            &self.message,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Failure] type.
///
/// Can be created using the [Failure::builder] method.
#[derive(Debug)]
#[must_use]
pub struct FailureBuilder<State>(State);

impl<
    
> FailureBuilder
<
    (
    
    )
>
{
    /// Setter for the [`message` field](Failure#structfield.message).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn message<T0>(self, value: T0) -> FailureBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAs<::planus::Offset<str>>
    {
        FailureBuilder((
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
> FailureBuilder<(
    
        T0,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Failure].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Failure>
        where Self: ::planus::WriteAsOffset<Failure>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
> ::planus::WriteAs<::planus::Offset<Failure>> for FailureBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Failure>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Failure> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
> ::planus::WriteAsOptional<::planus::Offset<Failure>> for FailureBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Failure>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Failure>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAs<::planus::Offset<str>>,
    
> ::planus::WriteAsOffset<Failure> for FailureBuilder<(
    
        T0,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Failure> {
        
        let (
            
                v0,
            
        ) = &self.0;Failure::create(
            builder,
            
                v0,
            
        )
    }
}

/// Reference to a deserialized [Failure].
#[derive(Copy, Clone)]
pub struct FailureRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> FailureRef<'a> {
    
        /// Getter for the [`message` field](Failure#structfield.message).
        #[inline]
        pub fn message(&self) -> ::planus::Result<&'a ::core::primitive::str> {
            
            
              
              self.0.access_required(0, "Failure", "message")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for FailureRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("FailureRef");
        f.field("message", &self.message());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<FailureRef<'a>> for Failure {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: FailureRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            message: ::core::convert::Into::into(value.message()?),
        })
    }
}

impl<'a> ::planus::TableRead<'a> for FailureRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for FailureRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[FailureRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Failure>> for Failure {
    type Value = ::planus::Offset<Failure>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Failure>],
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

impl<'a> ::planus::ReadAsRoot<'a> for FailureRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[FailureRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Ping` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `Ping` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:30`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Ping{
    
        /// The field `sequence` in the table `Ping`
        pub sequence: u64,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Ping {
    fn default() -> Self {
        Self {
        sequence: 0,
        
        }
    }
}


impl Ping {
    /// Creates a [PingBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> PingBuilder<()> {
        PingBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_sequence: impl ::planus::WriteAsDefault<u64, u64>,
        
    ) -> ::planus::Offset<Self> {let prepared_sequence = field_sequence.prepare(builder, &0);

        let mut table_writer: ::planus::table_writer::TableWriter::<6> = ::core::default::Default::default();
        if prepared_sequence.is_some() {table_writer.write_entry::<u64>(0);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_sequence) = prepared_sequence {object_writer.write::<_, _, 8>(&prepared_sequence);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Ping>> for Ping {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Ping> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Ping>> for Ping {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Ping>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Ping> for Ping {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Ping> {
        Ping::create(
            builder,
        
            
            self.sequence,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Ping] type.
///
/// Can be created using the [Ping::builder] method.
#[derive(Debug)]
#[must_use]
pub struct PingBuilder<State>(State);

impl<
    
> PingBuilder
<
    (
    
    )
>
{
    /// Setter for the [`sequence` field](Ping#structfield.sequence).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn sequence<T0>(self, value: T0) -> PingBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsDefault<u64, u64>
    {
        PingBuilder((
            
            value,
        ))
    }

    
    /// Sets the [`sequence` field](Ping#structfield.sequence) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn sequence_as_default(self) -> PingBuilder<(
        
        ::planus::DefaultValue,
    )>
    {
        self.sequence(::planus::DefaultValue)
    }
    

    
}



impl<
    
        T0,
    
> PingBuilder<(
    
        T0,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Ping].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Ping>
        where Self: ::planus::WriteAsOffset<Ping>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAs<::planus::Offset<Ping>> for PingBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Ping>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Ping> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAsOptional<::planus::Offset<Ping>> for PingBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Ping>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Ping>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAsOffset<Ping> for PingBuilder<(
    
        T0,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Ping> {
        
        let (
            
                v0,
            
        ) = &self.0;Ping::create(
            builder,
            
                v0,
            
        )
    }
}

/// Reference to a deserialized [Ping].
#[derive(Copy, Clone)]
pub struct PingRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> PingRef<'a> {
    
        /// Getter for the [`sequence` field](Ping#structfield.sequence).
        #[inline]
        pub fn sequence(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(0, "Ping", "sequence")
              
            
            ?.unwrap_or(0))
            
        }
    
}

impl<'a> ::core::fmt::Debug for PingRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("PingRef");
        f.field("sequence", &self.sequence());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<PingRef<'a>> for Ping {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: PingRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            sequence: ::core::convert::TryInto::try_into(value.sequence()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for PingRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for PingRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[PingRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Ping>> for Ping {
    type Value = ::planus::Offset<Ping>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Ping>],
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

impl<'a> ::planus::ReadAsRoot<'a> for PingRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[PingRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Pong` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `Pong` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:31`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Pong{
    
        /// The field `sequence` in the table `Pong`
        pub sequence: u64,}


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Pong {
    fn default() -> Self {
        Self {
        sequence: 0,
        
        }
    }
}


impl Pong {
    /// Creates a [PongBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> PongBuilder<()> {
        PongBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_sequence: impl ::planus::WriteAsDefault<u64, u64>,
        
    ) -> ::planus::Offset<Self> {let prepared_sequence = field_sequence.prepare(builder, &0);

        let mut table_writer: ::planus::table_writer::TableWriter::<6> = ::core::default::Default::default();
        if prepared_sequence.is_some() {table_writer.write_entry::<u64>(0);}

        unsafe {
            table_writer.finish(builder, |object_writer| {if let ::core::option::Option::Some(prepared_sequence) = prepared_sequence {object_writer.write::<_, _, 8>(&prepared_sequence);}
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Pong>> for Pong {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Pong> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Pong>> for Pong {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Pong>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Pong> for Pong {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Pong> {
        Pong::create(
            builder,
        
            
            self.sequence,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Pong] type.
///
/// Can be created using the [Pong::builder] method.
#[derive(Debug)]
#[must_use]
pub struct PongBuilder<State>(State);

impl<
    
> PongBuilder
<
    (
    
    )
>
{
    /// Setter for the [`sequence` field](Pong#structfield.sequence).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn sequence<T0>(self, value: T0) -> PongBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsDefault<u64, u64>
    {
        PongBuilder((
            
            value,
        ))
    }

    
    /// Sets the [`sequence` field](Pong#structfield.sequence) to the default value.
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn sequence_as_default(self) -> PongBuilder<(
        
        ::planus::DefaultValue,
    )>
    {
        self.sequence(::planus::DefaultValue)
    }
    

    
}



impl<
    
        T0,
    
> PongBuilder<(
    
        T0,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Pong].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Pong>
        where Self: ::planus::WriteAsOffset<Pong>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAs<::planus::Offset<Pong>> for PongBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Pong>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Pong> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAsOptional<::planus::Offset<Pong>> for PongBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Pong>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Pong>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsDefault<u64, u64>,
    
> ::planus::WriteAsOffset<Pong> for PongBuilder<(
    
        T0,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Pong> {
        
        let (
            
                v0,
            
        ) = &self.0;Pong::create(
            builder,
            
                v0,
            
        )
    }
}

/// Reference to a deserialized [Pong].
#[derive(Copy, Clone)]
pub struct PongRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> PongRef<'a> {
    
        /// Getter for the [`sequence` field](Pong#structfield.sequence).
        #[inline]
        pub fn sequence(&self) -> ::planus::Result<u64> {
             ::core::result::Result::Ok( 
            
              
              self.0.access(0, "Pong", "sequence")
              
            
            ?.unwrap_or(0))
            
        }
    
}

impl<'a> ::core::fmt::Debug for PongRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("PongRef");
        f.field("sequence", &self.sequence());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<PongRef<'a>> for Pong {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: PongRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            sequence: ::core::convert::TryInto::try_into(value.sequence()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for PongRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for PongRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[PongRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Pong>> for Pong {
    type Value = ::planus::Offset<Pong>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Pong>],
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

impl<'a> ::planus::ReadAsRoot<'a> for PongRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[PongRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The table `Close` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `Close` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:32`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Close{
    }


#[allow(clippy::derivable_impls)]
impl ::core::default::Default for Close {
    fn default() -> Self {
        Self {
        
        }
    }
}


impl Close {
    /// Creates a [CloseBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> CloseBuilder<()> {
        CloseBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        
    ) -> ::planus::Offset<Self> {

        let table_writer: ::planus::table_writer::TableWriter::<4> = ::core::default::Default::default();
        unsafe {
            table_writer.finish(builder, |_table_writer| {});
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Close>> for Close {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Close> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Close>> for Close {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Close>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Close> for Close {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Close> {
        Close::create(
            builder,
        
        )
    }
}

/// Builder for serializing an instance of the [Close] type.
///
/// Can be created using the [Close::builder] method.
#[derive(Debug)]
#[must_use]
pub struct CloseBuilder<State>(State);



impl<
    
> CloseBuilder<(
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Close].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Close>
        where Self: ::planus::WriteAsOffset<Close>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
> ::planus::WriteAs<::planus::Offset<Close>> for CloseBuilder<(
    
)> {
    type Prepared = ::planus::Offset<Close>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Close> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
> ::planus::WriteAsOptional<::planus::Offset<Close>> for CloseBuilder<(
    
)> {
    type Prepared = ::planus::Offset<Close>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Close>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
> ::planus::WriteAsOffset<Close> for CloseBuilder<(
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Close> {
        Close::create(
            builder,
            
        )
    }
}

/// Reference to a deserialized [Close].
#[derive(Copy, Clone)]
pub struct CloseRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> CloseRef<'a> {
    
}

impl<'a> ::core::fmt::Debug for CloseRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("CloseRef");
        
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<CloseRef<'a>> for Close {
    type Error = ::planus::Error;


    fn try_from(_value: CloseRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            
        })
    }
}

impl<'a> ::planus::TableRead<'a> for CloseRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for CloseRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[CloseRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Close>> for Close {
    type Value = ::planus::Offset<Close>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Close>],
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

impl<'a> ::planus::ReadAsRoot<'a> for CloseRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[CloseRef]",
            "read_as_root",
            0,
        ))
    }
}

    
/// The union `Control` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Union `Control` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:33`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize)]
pub enum Control{
    
        /// The variant of type `Hello` in the union `Control`
        Hello(::planus::alloc::boxed::Box<self::Hello>),
    
        /// The variant of type `Welcome` in the union `Control`
        Welcome(::planus::alloc::boxed::Box<self::Welcome>),
    
        /// The variant of type `Failure` in the union `Control`
        Failure(::planus::alloc::boxed::Box<self::Failure>),
    
        /// The variant of type `Ping` in the union `Control`
        Ping(::planus::alloc::boxed::Box<self::Ping>),
    
        /// The variant of type `Pong` in the union `Control`
        Pong(::planus::alloc::boxed::Box<self::Pong>),
    
        /// The variant of type `Close` in the union `Control`
        Close(::planus::alloc::boxed::Box<self::Close>),
    
}


impl Control {
    /// Creates a [ControlBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> ControlBuilder<::planus::Uninitialized> {
        ControlBuilder(::planus::Uninitialized)
    }

    #[inline]
    pub fn create_hello(
      builder: &mut ::planus::Builder,
      value: impl ::planus::WriteAsOffset<self::Hello>,
    ) -> ::planus::UnionOffset<Self> {
        ::planus::UnionOffset::new(1, value.prepare(builder).downcast())
    }

    #[inline]
    pub fn create_welcome(
      builder: &mut ::planus::Builder,
      value: impl ::planus::WriteAsOffset<self::Welcome>,
    ) -> ::planus::UnionOffset<Self> {
        ::planus::UnionOffset::new(2, value.prepare(builder).downcast())
    }

    #[inline]
    pub fn create_failure(
      builder: &mut ::planus::Builder,
      value: impl ::planus::WriteAsOffset<self::Failure>,
    ) -> ::planus::UnionOffset<Self> {
        ::planus::UnionOffset::new(3, value.prepare(builder).downcast())
    }

    #[inline]
    pub fn create_ping(
      builder: &mut ::planus::Builder,
      value: impl ::planus::WriteAsOffset<self::Ping>,
    ) -> ::planus::UnionOffset<Self> {
        ::planus::UnionOffset::new(4, value.prepare(builder).downcast())
    }

    #[inline]
    pub fn create_pong(
      builder: &mut ::planus::Builder,
      value: impl ::planus::WriteAsOffset<self::Pong>,
    ) -> ::planus::UnionOffset<Self> {
        ::planus::UnionOffset::new(5, value.prepare(builder).downcast())
    }

    #[inline]
    pub fn create_close(
      builder: &mut ::planus::Builder,
      value: impl ::planus::WriteAsOffset<self::Close>,
    ) -> ::planus::UnionOffset<Self> {
        ::planus::UnionOffset::new(6, value.prepare(builder).downcast())
    }

    
}



impl ::planus::WriteAsUnion<Control> for Control {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Self> {
        match self {
            Self::Hello(value) => Self::create_hello(builder, value),
            Self::Welcome(value) => Self::create_welcome(builder, value),
            Self::Failure(value) => Self::create_failure(builder, value),
            Self::Ping(value) => Self::create_ping(builder, value),
            Self::Pong(value) => Self::create_pong(builder, value),
            Self::Close(value) => Self::create_close(builder, value),
            
        }
    }
}


impl ::planus::WriteAsOptionalUnion<Control> for Control {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Self>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}

/// Builder for serializing an instance of the [Control] type.
///
/// Can be created using the [Control::builder] method.
#[derive(Debug)]
#[must_use]
pub struct ControlBuilder<T>(T);

impl ControlBuilder<::planus::Uninitialized> {
    /// Creates an instance of the [`Hello` variant](Control#variant.Hello).
    #[inline]
    pub fn hello<T>(
        self,
        value: T,
    ) -> ControlBuilder<::planus::Initialized<1, T>>
        where T: ::planus::WriteAsOffset<self::Hello> {
        ControlBuilder(::planus::Initialized(value))
    }

    /// Creates an instance of the [`Welcome` variant](Control#variant.Welcome).
    #[inline]
    pub fn welcome<T>(
        self,
        value: T,
    ) -> ControlBuilder<::planus::Initialized<2, T>>
        where T: ::planus::WriteAsOffset<self::Welcome> {
        ControlBuilder(::planus::Initialized(value))
    }

    /// Creates an instance of the [`Failure` variant](Control#variant.Failure).
    #[inline]
    pub fn failure<T>(
        self,
        value: T,
    ) -> ControlBuilder<::planus::Initialized<3, T>>
        where T: ::planus::WriteAsOffset<self::Failure> {
        ControlBuilder(::planus::Initialized(value))
    }

    /// Creates an instance of the [`Ping` variant](Control#variant.Ping).
    #[inline]
    pub fn ping<T>(
        self,
        value: T,
    ) -> ControlBuilder<::planus::Initialized<4, T>>
        where T: ::planus::WriteAsOffset<self::Ping> {
        ControlBuilder(::planus::Initialized(value))
    }

    /// Creates an instance of the [`Pong` variant](Control#variant.Pong).
    #[inline]
    pub fn pong<T>(
        self,
        value: T,
    ) -> ControlBuilder<::planus::Initialized<5, T>>
        where T: ::planus::WriteAsOffset<self::Pong> {
        ControlBuilder(::planus::Initialized(value))
    }

    /// Creates an instance of the [`Close` variant](Control#variant.Close).
    #[inline]
    pub fn close<T>(
        self,
        value: T,
    ) -> ControlBuilder<::planus::Initialized<6, T>>
        where T: ::planus::WriteAsOffset<self::Close> {
        ControlBuilder(::planus::Initialized(value))
    }

    
}

impl<const N: u8, T> ControlBuilder<::planus::Initialized<N, T>> {
    /// Finish writing the builder to get an [UnionOffset](::planus::UnionOffset) to a serialized [Control].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Control>
        where Self: ::planus::WriteAsUnion<Control>
    {
        ::planus::WriteAsUnion::prepare(&self, builder)
    }
}

impl<T> ::planus::WriteAsUnion<Control> for ControlBuilder<::planus::Initialized<1, T>>
    where T: ::planus::WriteAsOffset<self::Hello>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Control> {
        ::planus::UnionOffset::new(1, (self.0).0.prepare(builder).downcast())
    }
}

impl<T> ::planus::WriteAsOptionalUnion<Control> for ControlBuilder<::planus::Initialized<1, T>>
    where T: ::planus::WriteAsOffset<self::Hello>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Control>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}
impl<T> ::planus::WriteAsUnion<Control> for ControlBuilder<::planus::Initialized<2, T>>
    where T: ::planus::WriteAsOffset<self::Welcome>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Control> {
        ::planus::UnionOffset::new(2, (self.0).0.prepare(builder).downcast())
    }
}

impl<T> ::planus::WriteAsOptionalUnion<Control> for ControlBuilder<::planus::Initialized<2, T>>
    where T: ::planus::WriteAsOffset<self::Welcome>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Control>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}
impl<T> ::planus::WriteAsUnion<Control> for ControlBuilder<::planus::Initialized<3, T>>
    where T: ::planus::WriteAsOffset<self::Failure>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Control> {
        ::planus::UnionOffset::new(3, (self.0).0.prepare(builder).downcast())
    }
}

impl<T> ::planus::WriteAsOptionalUnion<Control> for ControlBuilder<::planus::Initialized<3, T>>
    where T: ::planus::WriteAsOffset<self::Failure>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Control>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}
impl<T> ::planus::WriteAsUnion<Control> for ControlBuilder<::planus::Initialized<4, T>>
    where T: ::planus::WriteAsOffset<self::Ping>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Control> {
        ::planus::UnionOffset::new(4, (self.0).0.prepare(builder).downcast())
    }
}

impl<T> ::planus::WriteAsOptionalUnion<Control> for ControlBuilder<::planus::Initialized<4, T>>
    where T: ::planus::WriteAsOffset<self::Ping>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Control>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}
impl<T> ::planus::WriteAsUnion<Control> for ControlBuilder<::planus::Initialized<5, T>>
    where T: ::planus::WriteAsOffset<self::Pong>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Control> {
        ::planus::UnionOffset::new(5, (self.0).0.prepare(builder).downcast())
    }
}

impl<T> ::planus::WriteAsOptionalUnion<Control> for ControlBuilder<::planus::Initialized<5, T>>
    where T: ::planus::WriteAsOffset<self::Pong>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Control>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}
impl<T> ::planus::WriteAsUnion<Control> for ControlBuilder<::planus::Initialized<6, T>>
    where T: ::planus::WriteAsOffset<self::Close>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::UnionOffset<Control> {
        ::planus::UnionOffset::new(6, (self.0).0.prepare(builder).downcast())
    }
}

impl<T> ::planus::WriteAsOptionalUnion<Control> for ControlBuilder<::planus::Initialized<6, T>>
    where T: ::planus::WriteAsOffset<self::Close>
{
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::UnionOffset<Control>> {
        ::core::option::Option::Some(::planus::WriteAsUnion::prepare(self, builder))
    }
}


/// Reference to a deserialized [Control].
#[derive(Copy, Clone, Debug)]
pub enum ControlRef<'a>{
    Hello(self::HelloRef<'a>),
    Welcome(self::WelcomeRef<'a>),
    Failure(self::FailureRef<'a>),
    Ping(self::PingRef<'a>),
    Pong(self::PongRef<'a>),
    Close(self::CloseRef<'a>),
    
}


impl<'a> ::core::convert::TryFrom<ControlRef<'a>> for Control {
    type Error = ::planus::Error;

    fn try_from(value: ControlRef<'a>) -> ::planus::Result<Self> {
        ::core::result::Result::Ok(match value {
            
                ControlRef::Hello(value) => Self::Hello(::planus::alloc::boxed::Box::new(::core::convert::TryFrom::try_from(value)?)),
                
            
                ControlRef::Welcome(value) => Self::Welcome(::planus::alloc::boxed::Box::new(::core::convert::TryFrom::try_from(value)?)),
                
            
                ControlRef::Failure(value) => Self::Failure(::planus::alloc::boxed::Box::new(::core::convert::TryFrom::try_from(value)?)),
                
            
                ControlRef::Ping(value) => Self::Ping(::planus::alloc::boxed::Box::new(::core::convert::TryFrom::try_from(value)?)),
                
            
                ControlRef::Pong(value) => Self::Pong(::planus::alloc::boxed::Box::new(::core::convert::TryFrom::try_from(value)?)),
                
            
                ControlRef::Close(value) => Self::Close(::planus::alloc::boxed::Box::new(::core::convert::TryFrom::try_from(value)?)),
                
            
        })
    }
}



impl<'a> ::planus::TableReadUnion<'a> for ControlRef<'a> {
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, tag: u8, field_offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        match tag {
            1 => ::core::result::Result::Ok(Self::Hello(::planus::TableRead::from_buffer(buffer, field_offset)?)),2 => ::core::result::Result::Ok(Self::Welcome(::planus::TableRead::from_buffer(buffer, field_offset)?)),3 => ::core::result::Result::Ok(Self::Failure(::planus::TableRead::from_buffer(buffer, field_offset)?)),4 => ::core::result::Result::Ok(Self::Ping(::planus::TableRead::from_buffer(buffer, field_offset)?)),5 => ::core::result::Result::Ok(Self::Pong(::planus::TableRead::from_buffer(buffer, field_offset)?)),6 => ::core::result::Result::Ok(Self::Close(::planus::TableRead::from_buffer(buffer, field_offset)?)),_ => ::core::result::Result::Err(::planus::errors::ErrorKind::UnknownUnionTag { tag }),
        }
    }
}



impl<'a> ::planus::VectorReadUnion<'a> for ControlRef<'a> {
    const VECTOR_NAME: &'static str = "[ControlRef]";
}


    
/// The table `Message` in the namespace `remote_wire`
/// 
/// Generated from these locations:
/// * Table `Message` in the file `crates/bkpr-protocol/src/remote_wire/remote.fbs:34`
#[derive(Clone, Debug, PartialEq, PartialOrd,
Eq, Ord, Hash,
::serde::Serialize, ::serde::Deserialize
)]
pub struct Message{
    
        /// The field `control` in the table `Message`
        pub control: self::Control,}



impl Message {
    /// Creates a [MessageBuilder] for serializing an instance of this table.
    #[inline]
    pub fn builder() -> MessageBuilder<()> {
        MessageBuilder(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        builder: &mut ::planus::Builder,
        field_control: impl ::planus::WriteAsUnion<self::Control>,
        
    ) -> ::planus::Offset<Self> {let prepared_control = field_control.prepare(builder);

        let mut table_writer: ::planus::table_writer::TableWriter::<8> = ::core::default::Default::default();
        table_writer.write_entry::<::planus::Offset<self::Control>>(1);table_writer.write_entry::<u8>(0);

        unsafe {
            table_writer.finish(builder, |object_writer| {object_writer.write::<_, _, 4>(&prepared_control.offset());object_writer.write::<_, _, 1>(&prepared_control.tag());
                
            });
        }builder.current_offset()
    }
}

impl ::planus::WriteAs<::planus::Offset<Message>> for Message {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Message> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl ::planus::WriteAsOptional<::planus::Offset<Message>> for Message {
    type Prepared = ::planus::Offset<Self>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Message>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl ::planus::WriteAsOffset<Message> for Message {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Message> {
        Message::create(
            builder,
        
            
            &self.control,
            
        
        )
    }
}

/// Builder for serializing an instance of the [Message] type.
///
/// Can be created using the [Message::builder] method.
#[derive(Debug)]
#[must_use]
pub struct MessageBuilder<State>(State);

impl<
    
> MessageBuilder
<
    (
    
    )
>
{
    /// Setter for the [`control` field](Message#structfield.control).
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn control<T0>(self, value: T0) -> MessageBuilder<(
    
        T0,
    
    )>
    where T0: ::planus::WriteAsUnion<self::Control>
    {
        MessageBuilder((
            
            value,
        ))
    }

    

    
}



impl<
    
        T0,
    
> MessageBuilder<(
    
        T0,
    
)> {
    /// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Message].
    #[inline]
    pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Message>
        where Self: ::planus::WriteAsOffset<Message>
    {
        ::planus::WriteAsOffset::prepare(&self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsUnion<self::Control>,
    
> ::planus::WriteAs<::planus::Offset<Message>> for MessageBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Message>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Message> {
        ::planus::WriteAsOffset::prepare(self, builder)
    }
}

impl<
    
        T0: ::planus::WriteAsUnion<self::Control>,
    
> ::planus::WriteAsOptional<::planus::Offset<Message>> for MessageBuilder<(
    
        T0,
    
)> {
    type Prepared = ::planus::Offset<Message>;

    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::core::option::Option<::planus::Offset<Message>> {
        ::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
    }
}

impl<
    
        T0: ::planus::WriteAsUnion<self::Control>,
    
> ::planus::WriteAsOffset<Message> for MessageBuilder<(
    
        T0,
    
)> {
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Message> {
        
        let (
            
                v0,
            
        ) = &self.0;Message::create(
            builder,
            
                v0,
            
        )
    }
}

/// Reference to a deserialized [Message].
#[derive(Copy, Clone)]
pub struct MessageRef<'a>(
    #[allow(dead_code)]
    ::planus::table_reader::Table<'a>
);

impl<'a> MessageRef<'a> {
    
        /// Getter for the [`control` field](Message#structfield.control).
        #[inline]
        pub fn control(&self) -> ::planus::Result<self::ControlRef<'a>> {
            
            
              
              self.0.access_union_required(0, "Message", "control")
              
            
            
            
        }
    
}

impl<'a> ::core::fmt::Debug for MessageRef<'a> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        let mut f = f.debug_struct("MessageRef");
        f.field("control", &self.control());
        f.finish()
    }
}

impl<'a> ::core::convert::TryFrom<MessageRef<'a>> for Message {
    type Error = ::planus::Error;


    #[allow(unreachable_code)]
    fn try_from(value: MessageRef<'a>) -> ::planus::Result<Self> {

        ::core::result::Result::Ok(Self {
            control: ::core::convert::TryInto::try_into(value.control()?)?,
        })
    }
}

impl<'a> ::planus::TableRead<'a> for MessageRef<'a> {
    #[inline]
    fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
        ::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
    }
}

impl<'a> ::planus::VectorReadInner<'a> for MessageRef<'a> {
    type Error = ::planus::Error;
    const STRIDE: usize = 4;

    unsafe fn from_buffer(buffer: ::planus::SliceWithStartOffset<'a>, offset: usize) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| error_kind.with_error_location(
            "[MessageRef]",
            "get",
            buffer.offset_from_start,
        ))
    }
}

/// # Safety
/// The planus compiler generates implementations that initialize
/// the bytes in `write_values`.
unsafe impl ::planus::VectorWrite<::planus::Offset<Message>> for Message {
    type Value = ::planus::Offset<Message>;
    const STRIDE: usize = 4;
    #[inline]
    fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
        ::planus::WriteAs::prepare(self, builder)
    }

    #[inline]
    unsafe fn write_values(
        values: &[::planus::Offset<Message>],
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

impl<'a> ::planus::ReadAsRoot<'a> for MessageRef<'a> {
    fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
        ::planus::TableRead::from_buffer(::planus::SliceWithStartOffset {
            buffer: slice,
            offset_from_start: 0,
        }, 0).map_err(|error_kind| error_kind.with_error_location(
            "[MessageRef]",
            "read_as_root",
            0,
        ))
    }
}

    }}