//! Telling a password question from other terminal output, in one place: Startup Commands stop
//! at one, Command Marks skip the answer to one, and a Startup Command's Wait for text that
//! names one would be a password saved in the clear.

/// "Password" in the languages sudo, su, ssh and passwd ask in. Matched anywhere in the
/// lowercased text, so "[sudo] Passwort für ana:" and "Enter passphrase for key" both count.
const WORDS: [&str; 26] = [
    "password",
    "passphrase",
    "passwort",
    "kennwort",
    "mot de passe",
    "contraseña",
    "senha",
    "wachtwoord",
    "hasło",
    "heslo",
    "lösenord",
    "adgangskode",
    "passord",
    "salasana",
    "jelszó",
    "kata sandi",
    "lozinka",
    "parola",
    "şifre",
    "пароль",
    "密码",
    "密碼",
    "パスワード",
    "암호",
    "비밀번호",
    "κωδικός πρόσβασης",
];

/// Whether `text` names a password, passphrase or PIN anywhere in it. "PIN" counts only as a
/// word of its own: "Enter PIN for 'YubiKey':", not "Spinning up" or "Mapping".
pub(crate) fn mentions_password(text: &str) -> bool {
    let text = text.to_lowercase();
    WORDS.iter().any(|word| text.contains(word))
        || text.split(|c: char| !c.is_alphanumeric()).any(|word| word == "pin")
}

/// Whether a terminal line is a question for a password ("[sudo] Passwort für ana: "): it
/// names one and ends in a colon, ASCII or fullwidth (Chinese and Japanese prompts).
pub(crate) fn asks_for_password(line: &str) -> bool {
    let line = line.trim_end();
    (line.ends_with(':') || line.ends_with('：')) && mentions_password(line)
}

#[cfg(test)]
mod tests {
    use super::{asks_for_password, mentions_password};

    #[test]
    fn password_questions_in_many_languages() {
        let questions = [
            "Password:",
            "password: ",
            "[sudo] password for ana:   ",
            "[sudo] Passwort für ana: ",
            "[sudo] Mot de passe de ana : ",
            "[sudo] contraseña para ana: ",
            "[sudo] Contraseña para ana: ",
            "[sudo] password di ana: ",
            "[sudo] wachtwoord voor ana: ",
            "[sudo] senha para ana: ",
            "[sudo] hasło użytkownika ana: ",
            "[sudo] пароль для ana: ",
            "[sudo] пароль до ana: ",
            "[sudo] heslo pro ana: ",
            "[sudo] lösenord för ana: ",
            "[sudo] adgangskode for ana: ",
            "[sudo] ana için parola: ",
            "[sudo] kata sandi untuk ana: ",
            "[sudo] lozinka za ana: ",
            "[sudo] ana 的密码：",
            "[sudo] ana 的密碼：",
            "[sudo] ana のパスワード: ",
            "[sudo] ana 암호: ",
            "[sudo: authenticate] Password: ",
            "Passwort: ",
            "Mot de passe : ",
            "Contraseña:",
            "Senha:",
            "Hasło:",
            "Пароль: ",
            "ana@web01's password: ",
            "ana@web01 的密码：",
            "Enter passphrase for key '/root/.ssh/id_ed25519':",
            "Enter PIN for 'PIV Card Holder pin (PIV_II)': ",
            "PIN:",
            "Current password: ",
            "New password: ",
            "Retype new password: ",
        ];
        for line in questions {
            assert!(asks_for_password(line), "{line:?} asks for a password");
        }
    }

    #[test]
    fn other_lines_are_not_password_questions() {
        let lines = [
            "Spinning up:",
            "Mapping:",
            "Pinned packages:",
            "Shipping:",
            "CPU 50% used",
            "ana@web01:~$ ",
            "[ops@web01 ~]$ ",
            "export PASSWORD_FILE=/tmp/x",
            "$ cat password.txt",
            "Password changed.",
            "passwd: password updated successfully",
            "Sorry, try again.",
            "",
        ];
        for line in lines {
            assert!(!asks_for_password(line), "{line:?} does not ask for a password");
        }
    }

    #[test]
    fn pin_counts_only_as_a_word() {
        assert!(mentions_password("Enter PIN"));
        assert!(mentions_password("pin-code"));
        assert!(!mentions_password("spinning"));
        assert!(!mentions_password("mapping"));
        assert!(!mentions_password("pinned"));
    }
}
