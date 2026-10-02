import Foundation

/// The setup step's strength meter (the concept's `siStrength`): a level 0 to 5, the share of the track it fills, a
/// word and a hint. The dashboard's own rule is 8 code points to 72 bytes.
public struct PasswordStrength: Equatable, Sendable {
  public let level: Int
  public let fill: Double
  public let word: String
  public let hint: String

  public static func evaluate(_ password: String) -> PasswordStrength {
    if password.isEmpty { return PasswordStrength(level: 0, fill: 0, word: "", hint: "At least 8 characters. Longer is stronger.") }
    let length = password.count
    let kinds = ["[a-z]", "[A-Z]", "[0-9]", "[^A-Za-z0-9]"].filter { password.range(of: $0, options: .regularExpression) != nil }.count
    if length < 8 {
      let more = 8 - length
      return PasswordStrength(level: 1, fill: Double(14 + length * 2) / 100, word: "Too short",
        hint: "\(more) more character\(more == 1 ? "" : "s") needed")
    }
    if password.utf8.count > 72 { return PasswordStrength(level: 1, fill: 1, word: "Too long", hint: "Keep it within 72 bytes") }
    if password.range(of: "^(.)\\1+$", options: .regularExpression) != nil
      || password.range(of: "^(password|12345678|qwerty|letmein|admin)", options: [.regularExpression, .caseInsensitive]) != nil {
      return PasswordStrength(level: 2, fill: 0.30, word: "Weak", hint: "Easy to guess; avoid common words")
    }
    let score = (length >= 20 ? 3.0 : length >= 14 ? 2 : length >= 10 ? 1 : 0) + (kinds >= 3 ? 1 : 0) + (kinds >= 2 ? 0.5 : 0)
    if score >= 3 { return PasswordStrength(level: 5, fill: 1, word: "Strong", hint: "\(length) characters, \(kinds) kinds") }
    if score >= 2 { return PasswordStrength(level: 4, fill: 0.80, word: "Good", hint: "\(length) characters; 14 or more is stronger") }
    if score >= 1 { return PasswordStrength(level: 3, fill: 0.58, word: "Fair", hint: "Add length: a few words with dashes works well") }
    return PasswordStrength(level: 2, fill: 0.36, word: "Weak", hint: "Add length or mix letters, digits and symbols")
  }

  /// The setup form's own checks before anything is sent: the field and its message, or nil.
  public static func setupProblem(username: String, password: String, confirm: String, code: String,
    codeRequired: Bool) -> (field: String, message: String)? {
    if username.range(of: "^[A-Za-z][A-Za-z0-9_-]{2,63}\\z", options: .regularExpression) == nil {
      return ("user", SignInCopy.usernameRule)
    }
    if password.count < 8 { return ("pass", SignInCopy.passwordShort) }
    if password.utf8.count > 72 { return ("pass", SignInCopy.passwordLong) }
    if confirm != password { return ("confirm", SignInCopy.confirmMismatch) }
    guard codeRequired else { return nil }
    let compact = code.filter { $0.isLetter || $0.isNumber }
    if code.trimmingCharacters(in: .whitespaces).isEmpty { return ("code", SignInCopy.enterCode) }
    if compact.count != 8 { return ("code", SignInCopy.wrongCode) }
    return nil
  }
}
