//! Shared memory between an APO and a process in the user's session.
//!
//! An APO runs inside `audiodg.exe`: session 0, as LocalService, usually as a
//! protected process. A named file mapping in the `Global\` namespace is the
//! one channel that both it and an ordinary desktop process can reach.
//!
//! - The **APO creates** the section ([`SharedSection::create`]). Creating a
//!   `Global\` object needs `SeCreateGlobalPrivilege`, which LocalService has
//!   and a non-elevated user process does not.
//! - The **host opens** it ([`SharedSection::open`]) and never creates it.
//!   Before mapping, it checks that the section is owned by a service identity
//!   and that its DACL only names expected principals, so a process that
//!   squatted the name first is refused.
//!
//! The DACL grants SYSTEM, Administrators, LocalService, NetworkService,
//! Interactive Users and the creator. Interactive Users is what lets the
//! desktop host in; it also means any process in the interactive session can
//! map the section, which is the trust boundary consumers must design for.
//!
//! The real-time callback only ever touches the mapped bytes; every call in
//! this module is for `initialize`/`lock` or the host, never `process`.

use std::ffi::c_void;

type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_ALL_ACCESS: u32 = 0xF001F;
const READ_CONTROL: u32 = 0x20000;
const ERROR_ALREADY_EXISTS: u32 = 183;
const SE_KERNEL_OBJECT: u32 = 6;
const OWNER_SECURITY_INFORMATION: u32 = 1;
const DACL_SECURITY_INFORMATION: u32 = 4;
const TOKEN_QUERY: u32 = 8;
const TOKEN_USER: u32 = 1;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// SYSTEM, LocalService, NetworkService: identities an APO section may be owned by.
const SERVICE_SIDS: [&str; 3] = ["S-1-5-18", "S-1-5-19", "S-1-5-20"];
/// Principals the section DACL may name (besides the current user).
const ALLOWED_TRUSTEES: [&str; 5] = ["S-1-5-18", "S-1-5-32-544", "S-1-5-19", "S-1-5-20", "S-1-5-4"];

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit_handle: i32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        file: Handle,
        attrs: *const SecurityAttributes,
        protect: u32,
        max_high: u32,
        max_low: u32,
        name: *const u16,
    ) -> Handle;
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn MapViewOfFile(mapping: Handle, access: u32, off_hi: u32, off_lo: u32, bytes: usize) -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetLastError() -> u32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetCurrentProcess() -> Handle;
}

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        length: *mut u32,
    ) -> i32;
    fn GetSecurityInfo(
        handle: Handle,
        kind: u32,
        info: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn IsValidAcl(acl: *const c_void) -> i32;
    fn GetAce(acl: *const c_void, index: u32, ace: *mut *mut c_void) -> i32;
    fn ConvertSidToStringSidW(sid: *const c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSidToSidW(text: *const u16, sid: *mut *mut c_void) -> i32;
    fn EqualSid(a: *const c_void, b: *const c_void) -> i32;
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(token: Handle, class: u32, data: *mut c_void, len: u32, needed: *mut u32) -> i32;
}

/// Why a section could not be created or opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionError {
    /// No section with that name exists yet (host side: the APO hasn't run).
    NotFound,
    /// A Win32 call failed; carries the call name and `GetLastError`.
    Os(&'static str, u32),
    /// The section exists but its owner or DACL is not the expected one.
    Untrusted(&'static str),
}

impl std::fmt::Display for SectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "shared section not found (is the APO running?)"),
            Self::Os(call, code) => write!(f, "{call} failed (Win32 error {code})"),
            Self::Untrusted(why) => write!(f, "refusing shared section: {why}"),
        }
    }
}

impl std::error::Error for SectionError {}

/// Who must own a section for [`SharedSection::open`] to accept it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerPolicy {
    /// SYSTEM, LocalService or NetworkService: a section made by an APO.
    Service,
    /// A service identity or the current user. For tests and same-session
    /// peers using `Local\` names.
    ServiceOrCurrentUser,
}

/// A mapped, named file mapping shared with an APO.
pub struct SharedSection {
    handle: Handle,
    ptr: *mut u8,
    len: usize,
    created: bool,
}

// The mapping is process-wide memory; synchronization is the consumer's
// protocol (atomics in the mapped layout).
unsafe impl Send for SharedSection {}
unsafe impl Sync for SharedSection {}

impl SharedSection {
    /// Create the section, or open it if it already exists (APO side).
    ///
    /// A pre-existing section is only accepted if it passes the same checks
    /// as [`open`](Self::open) with `policy`; new sections are zero-filled by
    /// the OS. [`created`](Self::created) tells the two cases apart.
    pub fn create(name: &str, len: usize, policy: OwnerPolicy) -> Result<Self, SectionError> {
        let wide = to_wide(name);
        let descriptor = ScopedDescriptor::new()?;
        let attrs = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: descriptor.0,
            inherit_handle: 0,
        };
        let size = len as u64;
        // SAFETY: valid NUL-terminated name and security attributes.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                &attrs,
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                wide.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(SectionError::Os("CreateFileMappingW", unsafe { GetLastError() }));
        }
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if existed {
            if let Err(e) = validate(handle, policy) {
                unsafe { CloseHandle(handle) };
                return Err(e);
            }
        }
        Self::map(handle, len, !existed)
    }

    /// Open an existing section (host side). Never creates one.
    pub fn open(name: &str, len: usize, policy: OwnerPolicy) -> Result<Self, SectionError> {
        let wide = to_wide(name);
        // SAFETY: valid NUL-terminated name.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS | READ_CONTROL, 0, wide.as_ptr()) };
        if handle.is_null() {
            let code = unsafe { GetLastError() };
            return Err(if code == 2 { SectionError::NotFound } else { SectionError::Os("OpenFileMappingW", code) });
        }
        if let Err(e) = validate(handle, policy) {
            unsafe { CloseHandle(handle) };
            return Err(e);
        }
        Self::map(handle, len, false)
    }

    fn map(handle: Handle, len: usize, created: bool) -> Result<Self, SectionError> {
        // SAFETY: `handle` is a live mapping handle; `len` bytes were requested
        // at creation (MapViewOfFile fails if the section is smaller).
        let ptr = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len) } as *mut u8;
        if ptr.is_null() {
            let code = unsafe { GetLastError() };
            unsafe { CloseHandle(handle) };
            return Err(SectionError::Os("MapViewOfFile", code));
        }
        Ok(Self { handle, ptr, len, created })
    }

    /// Start of the mapped bytes.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Mapped length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false; a zero-length mapping cannot be created.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when this call created the section (its bytes start zeroed).
    pub fn created(&self) -> bool {
        self.created
    }
}

impl Drop for SharedSection {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.ptr as *const c_void);
            CloseHandle(self.handle);
        }
    }
}

/// `Global\<prefix>.<endpoint_hash:016x>`: one section per endpoint, named
/// after [`crate::endpoint_hash`] so the host can find the APO's section.
pub fn endpoint_section_name(prefix: &str, endpoint_hash: u64) -> String {
    format!("Global\\{prefix}.{endpoint_hash:016x}")
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A LocalAlloc'd pointer (SID, descriptor, string) freed on drop.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

struct ScopedDescriptor(*mut c_void);

impl ScopedDescriptor {
    fn new() -> Result<Self, SectionError> {
        let creator = current_user_sid().ok_or(SectionError::Os("GetTokenInformation", 0))?;
        let sddl = format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GA;;;NS)(A;;GA;;;IU)(A;;GA;;;{})",
            creator.text
        );
        let wide = to_wide(&sddl);
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: valid SDDL string; the OS allocates `descriptor`.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(wide.as_ptr(), 1, &mut descriptor, std::ptr::null_mut())
        } == 0
        {
            return Err(SectionError::Os("ConvertStringSecurityDescriptorToSecurityDescriptorW", unsafe {
                GetLastError()
            }));
        }
        Ok(Self(descriptor))
    }
}

impl Drop for ScopedDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

struct UserSid {
    /// TOKEN_USER buffer; the SID pointer points into it.
    _buf: Vec<u8>,
    sid: *mut c_void,
    text: String,
}

fn current_user_sid() -> Option<UserSid> {
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut needed = 0;
        GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed);
        let mut buf = vec![0u8; needed as usize];
        let ok = GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), needed, &mut needed) != 0;
        CloseHandle(token);
        if !ok || buf.len() < std::mem::size_of::<*mut c_void>() {
            return None;
        }
        let sid = std::ptr::read_unaligned(buf.as_ptr().cast::<*mut c_void>());
        let text = sid_text(sid)?;
        Some(UserSid { _buf: buf, sid, text })
    }
}

unsafe fn sid_text(sid: *const c_void) -> Option<String> {
    let mut text = std::ptr::null_mut();
    if ConvertSidToStringSidW(sid, &mut text) == 0 {
        return None;
    }
    let _free = Local(text.cast());
    let mut len = 0;
    while *text.add(len) != 0 {
        len += 1;
    }
    Some(String::from_utf16_lossy(std::slice::from_raw_parts(text, len)))
}

fn well_known(text: &str) -> Option<Local> {
    let wide = to_wide(text);
    let mut sid = std::ptr::null_mut();
    (unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } != 0).then(|| Local(sid))
}

/// Accept a kernel object only if its owner matches `policy` and its DACL is
/// present, non-empty and made only of allow-ACEs for expected principals.
fn validate(handle: Handle, policy: OwnerPolicy) -> Result<(), SectionError> {
    unsafe {
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let rc = GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if rc != 0 {
            return Err(SectionError::Os("GetSecurityInfo", rc));
        }
        let _sd = Local(descriptor);
        if owner.is_null() {
            return Err(SectionError::Untrusted("no owner"));
        }
        if dacl.is_null() || IsValidAcl(dacl) == 0 {
            return Err(SectionError::Untrusted("NULL or invalid DACL"));
        }
        let me = current_user_sid().ok_or(SectionError::Os("GetTokenInformation", 0))?;
        let services: Vec<Local> = SERVICE_SIDS.iter().filter_map(|s| well_known(s)).collect();
        let owner_ok = services.iter().any(|s| EqualSid(owner, s.0) != 0)
            || (policy == OwnerPolicy::ServiceOrCurrentUser && EqualSid(owner, me.sid) != 0);
        if !owner_ok {
            return Err(SectionError::Untrusted("unexpected owner"));
        }
        let allowed: Vec<Local> = ALLOWED_TRUSTEES.iter().filter_map(|s| well_known(s)).collect();
        // ACL header: AclRevision u8, Sbz1 u8, AclSize u16, AceCount u16, Sbz2 u16.
        let count = std::ptr::read_unaligned((dacl as *const u8).add(4).cast::<u16>());
        if count == 0 {
            return Err(SectionError::Untrusted("empty DACL"));
        }
        for i in 0..count as u32 {
            let mut ace = std::ptr::null_mut();
            if GetAce(dacl, i, &mut ace) == 0 {
                return Err(SectionError::Untrusted("unreadable ACE"));
            }
            // ACCESS_ALLOWED_ACE: header (type u8, flags u8, size u16), mask u32, SID.
            let bytes = ace as *const u8;
            if *bytes != ACCESS_ALLOWED_ACE_TYPE {
                return Err(SectionError::Untrusted("non-allow ACE"));
            }
            if std::ptr::read_unaligned(bytes.add(2).cast::<u16>()) < 16 {
                return Err(SectionError::Untrusted("truncated ACE"));
            }
            let trustee = bytes.add(8) as *const c_void;
            let known = allowed.iter().any(|s| EqualSid(trustee, s.0) != 0) || EqualSid(trustee, me.sid) != 0;
            if !known {
                return Err(SectionError::Untrusted("DACL grants an unexpected principal"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(tag: &str) -> String {
        format!("Local\\windows-apo-shared-test-{}-{tag}", std::process::id())
    }

    #[test]
    fn create_then_open_shares_bytes() {
        let n = name("roundtrip");
        let a = SharedSection::create(&n, 4096, OwnerPolicy::ServiceOrCurrentUser).unwrap();
        assert!(a.created());
        let b = SharedSection::open(&n, 4096, OwnerPolicy::ServiceOrCurrentUser).unwrap();
        assert!(!b.created());
        unsafe {
            *a.as_ptr().add(100) = 0x5A;
            assert_eq!(*b.as_ptr().add(100), 0x5A);
        }
    }

    #[test]
    fn open_missing_is_not_found() {
        let r = SharedSection::open(&name("missing"), 4096, OwnerPolicy::Service);
        assert_eq!(r.err(), Some(SectionError::NotFound));
    }

    #[test]
    fn user_owned_section_is_refused_under_service_policy() {
        // What a squatter would produce: owned by an ordinary user.
        let n = name("squat");
        let _mine = SharedSection::create(&n, 4096, OwnerPolicy::ServiceOrCurrentUser).unwrap();
        let r = SharedSection::open(&n, 4096, OwnerPolicy::Service);
        assert_eq!(r.err(), Some(SectionError::Untrusted("unexpected owner")));
    }

    #[test]
    fn null_dacl_section_is_refused() {
        let n = name("nulldacl");
        let wide = to_wide(&n);
        let sddl = to_wide("D:NO_ACCESS_CONTROL");
        unsafe {
            let mut sd = std::ptr::null_mut();
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut()),
                0
            );
            let _sd = Local(sd);
            let attrs = SecurityAttributes {
                length: std::mem::size_of::<SecurityAttributes>() as u32,
                descriptor: sd,
                inherit_handle: 0,
            };
            let h = CreateFileMappingW(INVALID_HANDLE_VALUE, &attrs, PAGE_READWRITE, 0, 4096, wide.as_ptr());
            assert!(!h.is_null());
            let r = SharedSection::open(&n, 4096, OwnerPolicy::ServiceOrCurrentUser);
            assert_eq!(r.err(), Some(SectionError::Untrusted("NULL or invalid DACL")));
            CloseHandle(h);
        }
    }

    #[test]
    fn endpoint_hash_matches_the_cpp_side() {
        // Full IMMDevice id and bare subkey name hash the same, case-insensitively.
        let full = "{0.0.1.00000000}.{3F135177-AFFB-4090-BDEE-D19FBF1D9BDA}";
        let bare = "{3f135177-affb-4090-bdee-d19fbf1d9bda}";
        assert_eq!(crate::endpoint_hash(full), crate::endpoint_hash(bare));
        // FNV-1a 64 of the empty string is the offset basis.
        assert_eq!(crate::endpoint_hash(""), 14695981039346656037);
        assert_eq!(
            endpoint_section_name("X", 0xab),
            "Global\\X.00000000000000ab"
        );
    }
}
