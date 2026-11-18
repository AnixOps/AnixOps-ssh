import Foundation
import LocalAuthentication
import Security
import Tauri

// The Swift side of `easyssh-mobile-keys`: the commands in `bridge.rs`
// (`secureGet`, `boundOpen`, ...). Every failure is a rejection with a code
// Rust maps: `cancelled`, `invalidated`, `unavailable`, or `failed`.
//
// Two kinds of Keychain item:
//
// - plain items hold the vault's device key (or its passphrase-wrapped form)
//   with `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`: not readable while
//   the phone is locked, never in a backup, never moved to another phone;
// - the biometric copy has a `SecAccessControl` with `.biometryCurrentSet`:
//   the Secure Enclave releases it only after Face ID or Touch ID matched, and
//   adding a fingerprint or face invalidates it. Code that skips the prompt
//   gets an error from the Keychain, not the key.

private struct Args: Decodable {
  let service: String?
  let account: String?
  let value: String?
  let reason: String?
}

private struct ValueReply: Encodable { let value: String? }
private struct AvailableReply: Encodable { let available: Bool }
private struct PresentReply: Encodable { let present: Bool }

private struct Failure: Error {
  let code: String
  let message: String
}

private let boundService = "com.anixops.ssh.bound"

private func describe(_ status: OSStatus) -> String {
  return (SecCopyErrorMessageString(status, nil) as String?) ?? "Keychain error \(status)"
}

private func base(_ service: String, _ account: String) -> [String: Any] {
  return [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: service,
    kSecAttrAccount as String: account,
  ]
}

private func laCode(_ error: Error?) -> String {
  guard let error = error as? LAError else { return "failed" }
  switch error.code {
  case .userCancel, .systemCancel, .appCancel, .userFallback:
    return "cancelled"
  case .biometryLockout, .biometryNotAvailable, .biometryNotEnrolled, .passcodeNotSet:
    return "unavailable"
  default:
    return "failed"
  }
}

/// Shows the Face ID or Touch ID prompt and waits for the answer. Call it off
/// the main thread.
private func verify(_ context: LAContext, reason: String) -> Error? {
  let done = DispatchSemaphore(value: 0)
  var failure: Error?
  context.evaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, localizedReason: reason) {
    success, error in
    if !success { failure = error ?? Failure(code: "failed", message: "not verified") }
    done.signal()
  }
  done.wait()
  return failure
}

class DeviceKeyPlugin: Plugin {
  // ---- secure storage ------------------------------------------------------

  @objc public func secureGet(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(Args.self)
    guard let service = args.service, let account = args.account else {
      invoke.reject("missing argument", code: "failed")
      return
    }
    var query = base(service, account)
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var found: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &found)
    switch status {
    case errSecSuccess:
      let data = found as? Data
      invoke.resolve(ValueReply(value: data?.base64EncodedString()))
    case errSecItemNotFound:
      invoke.resolve(ValueReply(value: nil))
    default:
      invoke.reject(describe(status), code: "unavailable")
    }
  }

  @objc public func secureSet(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(Args.self)
    guard let service = args.service, let account = args.account,
      let text = args.value, let data = Data(base64Encoded: text)
    else {
      invoke.reject("missing argument", code: "failed")
      return
    }
    let attributes: [String: Any] = [
      kSecValueData as String: data,
      kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
    ]
    var status = SecItemUpdate(base(service, account) as CFDictionary, attributes as CFDictionary)
    if status == errSecItemNotFound {
      var add = base(service, account)
      add.merge(attributes) { _, new in new }
      status = SecItemAdd(add as CFDictionary, nil)
    }
    if status == errSecSuccess {
      invoke.resolve()
    } else {
      invoke.reject(describe(status), code: "unavailable")
    }
  }

  @objc public func secureDelete(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(Args.self)
    guard let service = args.service, let account = args.account else {
      invoke.reject("missing argument", code: "failed")
      return
    }
    let status = SecItemDelete(base(service, account) as CFDictionary)
    if status == errSecSuccess || status == errSecItemNotFound {
      invoke.resolve()
    } else {
      invoke.reject(describe(status), code: "unavailable")
    }
  }

  // ---- biometric-bound storage -----------------------------------------------

  @objc public func boundStatus(_ invoke: Invoke) throws {
    var error: NSError?
    let ready = LAContext().canEvaluatePolicy(
      .deviceOwnerAuthenticationWithBiometrics, error: &error)
    invoke.resolve(AvailableReply(available: ready))
  }

  /// Whether an item exists, without a prompt: attributes of a protected item
  /// can be read without the sensor, its data cannot.
  @objc public func boundContains(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(Args.self)
    guard let account = args.account else {
      invoke.reject("missing argument", code: "failed")
      return
    }
    let context = LAContext()
    context.interactionNotAllowed = true
    var query = base(boundService, account)
    query[kSecReturnAttributes as String] = true
    query[kSecUseAuthenticationContext as String] = context
    let status = SecItemCopyMatching(query as CFDictionary, nil)
    let present = status == errSecSuccess || status == errSecInteractionNotAllowed
    invoke.resolve(PresentReply(present: present))
  }

  @objc public func boundRemove(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(Args.self)
    guard let account = args.account else {
      invoke.reject("missing argument", code: "failed")
      return
    }
    let status = SecItemDelete(base(boundService, account) as CFDictionary)
    if status == errSecSuccess || status == errSecItemNotFound {
      invoke.resolve()
    } else {
      invoke.reject(describe(status), code: "unavailable")
    }
  }

  /// Turning the method on proves the sensor works first, then stores the
  /// copy under an access control that needs the current biometric set.
  @objc public func boundSeal(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(Args.self)
    guard let account = args.account, let text = args.value, let data = Data(base64Encoded: text)
    else {
      invoke.reject("missing argument", code: "failed")
      return
    }
    let reason = args.reason ?? ""
    DispatchQueue.global(qos: .userInitiated).async {
      let context = LAContext()
      var probe: NSError?
      guard context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: &probe)
      else {
        invoke.reject(
          probe?.localizedDescription ?? "no Face ID or Touch ID is set up", code: "unavailable")
        return
      }
      if let failure = verify(context, reason: reason) {
        invoke.reject(failure.localizedDescription, code: laCode(failure))
        return
      }
      var created: Unmanaged<CFError>?
      guard
        let access = SecAccessControlCreateWithFlags(
          nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, .biometryCurrentSet, &created)
      else {
        invoke.reject("the access control could not be created", code: "failed")
        return
      }
      SecItemDelete(base(boundService, account) as CFDictionary)
      var add = base(boundService, account)
      add[kSecAttrAccessControl as String] = access
      add[kSecValueData as String] = data
      add[kSecUseAuthenticationContext as String] = context
      let status = SecItemAdd(add as CFDictionary, nil)
      if status == errSecSuccess {
        invoke.resolve()
      } else {
        invoke.reject(describe(status), code: "failed")
      }
    }
  }

  /// Reading the item raises the system prompt; the Keychain returns the data
  /// only after the sensor matched.
  @objc public func boundOpen(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(Args.self)
    guard let account = args.account else {
      invoke.reject("missing argument", code: "failed")
      return
    }
    let reason = args.reason ?? ""
    DispatchQueue.global(qos: .userInitiated).async {
      let context = LAContext()
      context.localizedReason = reason
      var query = base(boundService, account)
      query[kSecReturnData as String] = true
      query[kSecMatchLimit as String] = kSecMatchLimitOne
      query[kSecUseAuthenticationContext as String] = context
      var found: CFTypeRef?
      let status = SecItemCopyMatching(query as CFDictionary, &found)
      switch status {
      case errSecSuccess:
        let data = found as? Data
        invoke.resolve(ValueReply(value: data?.base64EncodedString()))
      case errSecItemNotFound:
        invoke.resolve(ValueReply(value: nil))
      case errSecUserCanceled:
        invoke.reject("cancelled", code: "cancelled")
      case errSecAuthFailed:
        invoke.reject(describe(status), code: "failed")
      default:
        invoke.reject(describe(status), code: "failed")
      }
    }
  }
}

@_cdecl("init_plugin_device_key")
public func initDeviceKeyPlugin() -> Plugin {
  return DeviceKeyPlugin()
}
