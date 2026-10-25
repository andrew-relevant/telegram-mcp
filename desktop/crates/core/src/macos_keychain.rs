//! Login-keychain records that do not ask for the keychain password.
//!
//! An ad-hoc signed app cannot save "Always Allow" on the default keychain
//! ACL, so the password dialog comes back for every read. Items are created
//! with that prompt turned off. The service name is not the one the previous
//! build already stored: opening those items would show the dialog again.

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::ptr;

use crate::secrets::SecretStoreError;

/// Distinct from `secrets::SERVICE_NAME`, which names the items the stuck
/// dialog is already asking about.
const SERVICE: &str = "app.telegram-mcp.desktop.login";
const ERR_ITEM_NOT_FOUND: i32 = -25300;
const ERR_USER_CANCELED: i32 = -128;
/// Override the default so unsigned and ad-hoc clients are not prompted.
const PROMPT_WITHOUT_DIALOG: u16 = 0x0020 | 0x0080;
const UTF8: u32 = 0x0800_0100;
const GENERIC_PASSWORD: u32 = u32::from_be_bytes(*b"genp");
const ACCOUNT_ATTR: u32 = u32::from_be_bytes(*b"acct");
const SERVICE_ATTR: u32 = u32::from_be_bytes(*b"svce");

type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type SecAccessRef = *mut c_void;
type SecACLRef = *mut c_void;
type SecKeychainItemRef = *mut c_void;

#[repr(C)]
struct SecKeychainAttribute {
    tag: u32,
    length: u32,
    data: *mut c_void,
}

#[repr(C)]
struct SecKeychainAttributeList {
    count: u32,
    attr: *mut SecKeychainAttribute,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        num_bytes: isize,
        encoding: u32,
        is_external: u8,
    ) -> CFStringRef;
    fn CFRelease(value: *const c_void);
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const c_void;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecAccessCreate(
        descriptor: CFStringRef,
        trusted_list: CFArrayRef,
        access: *mut SecAccessRef,
    ) -> i32;
    fn SecAccessCopyACLList(access: SecAccessRef, acl_list: *mut CFArrayRef) -> i32;
    fn SecACLSetContents(
        acl: SecACLRef,
        application_list: CFArrayRef,
        description: CFStringRef,
        prompt_selector: u16,
    ) -> i32;
    fn SecKeychainItemCreateFromContent(
        item_class: u32,
        attr_list: *mut SecKeychainAttributeList,
        length: u32,
        data: *const c_void,
        keychain: *mut c_void,
        initial_access: SecAccessRef,
        item: *mut SecKeychainItemRef,
    ) -> i32;
    fn SecKeychainFindGenericPassword(
        keychain_or_array: *const c_void,
        service_length: u32,
        service: *const c_char,
        account_length: u32,
        account: *const c_char,
        password_length: *mut u32,
        password_data: *mut *mut c_void,
        item: *mut SecKeychainItemRef,
    ) -> i32;
    fn SecKeychainItemModifyAttributesAndData(
        item: SecKeychainItemRef,
        attr_list: *const SecKeychainAttributeList,
        length: u32,
        data: *const c_void,
    ) -> i32;
    fn SecKeychainItemDelete(item: SecKeychainItemRef) -> i32;
    fn SecKeychainItemFreeContent(
        attr_list: *mut SecKeychainAttributeList,
        data: *mut c_void,
    ) -> i32;
}

unsafe extern "C" {
    static _dispatch_main_q: u8;
    fn dispatch_sync_f(
        queue: *mut c_void,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
    fn pthread_main_np() -> c_int;
}

fn main_queue() -> *mut c_void {
    std::ptr::addr_of!(_dispatch_main_q).cast_mut().cast()
}

pub fn get(account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
    let account = account.to_owned();
    on_main(move || read_secret(&account))
}

pub fn set(account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
    let account = account.to_owned();
    let secret = secret.to_vec();
    on_main(move || write_secret(&account, &secret))
}

pub fn delete(account: &str) -> Result<(), SecretStoreError> {
    let account = account.to_owned();
    on_main(move || remove_secret(&account))
}

fn on_main<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    if unsafe { pthread_main_np() } != 0 {
        return work();
    }
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let task = Box::new(Task {
        work: Box::new(move || {
            let _ = sender.send(work());
        }),
    });
    unsafe {
        dispatch_sync_f(main_queue(), Box::into_raw(task).cast(), run_task);
    }
    receiver
        .recv()
        .expect("главный поток не вернул результат связки ключей")
}

struct Task {
    work: Box<dyn FnOnce() + Send>,
}

unsafe extern "C" fn run_task(context: *mut c_void) {
    let task = Box::from_raw(context.cast::<Task>());
    (task.work)();
}

fn read_secret(account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
    let mut length = 0u32;
    let mut data = ptr::null_mut();
    let status = unsafe {
        SecKeychainFindGenericPassword(
            ptr::null(),
            byte_len(SERVICE)?,
            SERVICE.as_ptr().cast(),
            byte_len(account)?,
            account.as_ptr().cast(),
            &mut length,
            &mut data,
            ptr::null_mut(),
        )
    };
    if status == ERR_ITEM_NOT_FOUND {
        return Ok(None);
    }
    if status != 0 {
        return Err(keychain_error(status));
    }
    let secret = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length as usize).to_vec() };
    unsafe {
        SecKeychainItemFreeContent(ptr::null_mut(), data);
    }
    Ok(Some(secret))
}

fn write_secret(account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
    match find_item(account)? {
        Some(item) => {
            let status = unsafe {
                SecKeychainItemModifyAttributesAndData(
                    item,
                    ptr::null(),
                    byte_len_of(secret.len())?,
                    secret.as_ptr().cast(),
                )
            };
            unsafe { CFRelease(item) };
            if status != 0 {
                return Err(keychain_error(status));
            }
            Ok(())
        }
        None => create_item(account, secret),
    }
}

fn remove_secret(account: &str) -> Result<(), SecretStoreError> {
    let Some(item) = find_item(account)? else {
        return Ok(());
    };
    let status = unsafe { SecKeychainItemDelete(item) };
    unsafe { CFRelease(item) };
    if status != 0 {
        return Err(keychain_error(status));
    }
    Ok(())
}

fn find_item(account: &str) -> Result<Option<SecKeychainItemRef>, SecretStoreError> {
    let mut item = ptr::null_mut();
    let status = unsafe {
        SecKeychainFindGenericPassword(
            ptr::null(),
            byte_len(SERVICE)?,
            SERVICE.as_ptr().cast(),
            byte_len(account)?,
            account.as_ptr().cast(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut item,
        )
    };
    if status == ERR_ITEM_NOT_FOUND {
        return Ok(None);
    }
    if status != 0 {
        return Err(keychain_error(status));
    }
    Ok(Some(item))
}

fn create_item(account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
    let access = open_access()?;
    let mut service_bytes = SERVICE.as_bytes().to_vec();
    let mut account_bytes = account.as_bytes().to_vec();
    let mut attributes = [
        SecKeychainAttribute {
            tag: SERVICE_ATTR,
            length: byte_len_of(service_bytes.len())?,
            data: service_bytes.as_mut_ptr().cast(),
        },
        SecKeychainAttribute {
            tag: ACCOUNT_ATTR,
            length: byte_len_of(account_bytes.len())?,
            data: account_bytes.as_mut_ptr().cast(),
        },
    ];
    let mut list = SecKeychainAttributeList {
        count: attributes.len() as u32,
        attr: attributes.as_mut_ptr(),
    };
    let status = unsafe {
        SecKeychainItemCreateFromContent(
            GENERIC_PASSWORD,
            &mut list,
            byte_len_of(secret.len())?,
            secret.as_ptr().cast(),
            ptr::null_mut(),
            access,
            ptr::null_mut(),
        )
    };
    unsafe { CFRelease(access) };
    if status != 0 {
        return Err(keychain_error(status));
    }
    Ok(())
}

fn open_access() -> Result<SecAccessRef, SecretStoreError> {
    let label = cf_string("Telegram MCP")?;
    let mut access = ptr::null_mut();
    let status = unsafe { SecAccessCreate(label.0, ptr::null(), &mut access) };
    if status != 0 {
        return Err(keychain_error(status));
    }
    let mut acls = ptr::null();
    let status = unsafe { SecAccessCopyACLList(access, &mut acls) };
    if status != 0 {
        unsafe { CFRelease(access) };
        return Err(keychain_error(status));
    }
    let count = unsafe { CFArrayGetCount(acls) };
    for index in 0..count {
        let acl = unsafe { CFArrayGetValueAtIndex(acls, index) } as SecACLRef;
        let status = unsafe { SecACLSetContents(acl, ptr::null(), label.0, PROMPT_WITHOUT_DIALOG) };
        if status != 0 {
            unsafe {
                CFRelease(acls);
                CFRelease(access);
            }
            return Err(keychain_error(status));
        }
    }
    unsafe { CFRelease(acls) };
    Ok(access)
}

struct CfString(*const c_void);

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string(text: &str) -> Result<CfString, SecretStoreError> {
    let value = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            text.as_ptr(),
            isize::try_from(text.len())
                .map_err(|_| SecretStoreError::new("слишком длинное имя"))?,
            UTF8,
            0,
        )
    };
    if value.is_null() {
        return Err(SecretStoreError::new("не удалось подготовить имя записи"));
    }
    Ok(CfString(value))
}

fn byte_len(text: &str) -> Result<u32, SecretStoreError> {
    byte_len_of(text.len())
}

fn byte_len_of(length: usize) -> Result<u32, SecretStoreError> {
    u32::try_from(length).map_err(|_| SecretStoreError::new("слишком длинная запись связки ключей"))
}

fn keychain_error(status: i32) -> SecretStoreError {
    if status == ERR_USER_CANCELED {
        return SecretStoreError::new("доступ к связке ключей отменён");
    }
    SecretStoreError::new(format!("связка ключей не ответила ({status})"))
}
