/// User and group management
///
/// Provides Unix-like user database (/etc/passwd format),
/// password checking, and credential management.
/// Passwords are stored as SHA-256 hashes (FIPS 180-4) with a per-user
/// random salt prepended ("$5$<hex-salt>$<hex-digest>" inspired layout).

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// User entry (passwd format: name:password_hash:uid:gid:comment:home:shell)
#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub password_hash: String,  // Empty string = no password
    pub uid: u32,
    pub gid: u32,
    pub comment: String,        // GECOS field
    pub home: String,
    pub shell: String,
}

/// Group entry (group format: name:password:gid:members)
#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

/// User database
pub struct UserDb {
    users: Vec<User>,
    groups: Vec<Group>,
}

impl UserDb {
    pub const fn new() -> Self {
        UserDb {
            users: Vec::new(),
            groups: Vec::new(),
        }
    }

    /// Initialize with default users and groups
    pub fn init_defaults(&mut self) {
        // Default groups
        self.groups.push(Group {
            name: String::from("root"),
            gid: 0,
            members: alloc::vec![String::from("root")],
        });
        self.groups.push(Group {
            name: String::from("users"),
            gid: 100,
            members: Vec::new(),
        });
        self.groups.push(Group {
            name: String::from("wheel"),
            gid: 10,
            members: alloc::vec![String::from("root")],
        });
        self.groups.push(Group {
            name: String::from("daemon"),
            gid: 1,
            members: Vec::new(),
        });
        self.groups.push(Group {
            name: String::from("nobody"),
            gid: 65534,
            members: Vec::new(),
        });

        // Default users
        self.users.push(User {
            name: String::from("root"),
            password_hash: String::new(), // no password by default
            uid: 0,
            gid: 0,
            comment: String::from("System Administrator"),
            home: String::from("/root"),
            shell: String::from("/bin/sh"),
        });
        self.users.push(User {
            name: String::from("daemon"),
            password_hash: String::from("*"), // locked
            uid: 1,
            gid: 1,
            comment: String::from("System Daemon"),
            home: String::from("/"),
            shell: String::from("/bin/false"),
        });
        self.users.push(User {
            name: String::from("nobody"),
            password_hash: String::from("*"), // locked
            uid: 65534,
            gid: 65534,
            comment: String::from("Nobody"),
            home: String::from("/"),
            shell: String::from("/bin/false"),
        });
    }

    /// Look up a user by name
    pub fn get_user(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|u| u.name == name)
    }

    /// Look up a user by UID
    pub fn get_user_by_uid(&self, uid: u32) -> Option<&User> {
        self.users.iter().find(|u| u.uid == uid)
    }

    /// Look up a group by name
    pub fn get_group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

    /// Look up a group by GID
    pub fn get_group_by_gid(&self, gid: u32) -> Option<&Group> {
        self.groups.iter().find(|g| g.gid == gid)
    }

    /// Check if a password matches for a user.  Constant-time comparison
    /// via SHA-256(salt || password) against the stored salted hash.
    pub fn check_password(&self, name: &str, password: &str) -> bool {
        if let Some(user) = self.get_user(name) {
            verify_hash(password, &user.password_hash)
        } else {
            false
        }
    }

    /// Add a new user
    pub fn add_user(&mut self, user: User) -> Result<(), &'static str> {
        if self.users.iter().any(|u| u.name == user.name) {
            return Err("User already exists");
        }
        if self.users.iter().any(|u| u.uid == user.uid) {
            return Err("UID already in use");
        }
        self.users.push(user);
        Ok(())
    }

    /// Set/change user password
    pub fn set_password(&mut self, name: &str, password: &str) -> Result<(), &'static str> {
        let user = self.users.iter_mut().find(|u| u.name == name)
            .ok_or("User not found")?;
        user.password_hash = if password.is_empty() {
            String::new()
        } else {
            simple_hash(password)
        };
        Ok(())
    }

    /// Generate /etc/passwd content
    pub fn to_passwd_string(&self) -> String {
        let mut s = String::new();
        for u in &self.users {
            s.push_str(&alloc::format!(
                "{}:x:{}:{}:{}:{}:{}\n",
                u.name, u.uid, u.gid, u.comment, u.home, u.shell
            ));
        }
        s
    }

    /// Generate /etc/group content
    pub fn to_group_string(&self) -> String {
        let mut s = String::new();
        for g in &self.groups {
            let members = g.members.join(",");
            s.push_str(&alloc::format!("{}:x:{}:{}\n", g.name, g.gid, members));
        }
        s
    }

    /// Get all groups a user belongs to
    pub fn user_groups(&self, name: &str) -> Vec<&Group> {
        let mut result = Vec::new();
        // Primary group
        if let Some(user) = self.get_user(name) {
            if let Some(group) = self.get_group_by_gid(user.gid) {
                result.push(group);
            }
        }
        // Additional groups
        for group in &self.groups {
            if group.members.iter().any(|m| m == name) {
                if !result.iter().any(|g| g.gid == group.gid) {
                    result.push(group);
                }
            }
        }
        result
    }

    /// Get list of all users
    pub fn users(&self) -> &[User] {
        &self.users
    }
}

/// Hash a password as SHA-256(salt || password) and produce a
/// `$sha256$<hex-salt>$<hex-digest>` string we can compare on lookup.
/// The salt comes from the TSC + a counter so two equal passwords for
/// different users produce different stored hashes.
fn simple_hash(input: &str) -> String {
    use core::sync::atomic::{AtomicU64, Ordering};
    static SALT_COUNTER: AtomicU64 = AtomicU64::new(0);
    let salt_seed = (crate::task::timer::current_ticks()
        ^ SALT_COUNTER.fetch_add(1, Ordering::Relaxed))
        .wrapping_mul(0x9E3779B97F4A7C15);
    let salt = alloc::format!("{:016x}", salt_seed);

    let mut sha = crate::sha256::Sha256::new();
    sha.update(salt.as_bytes());
    sha.update(b"|");
    sha.update(input.as_bytes());
    let digest = sha.finalize();

    alloc::format!("$sha256${}${}", salt, crate::sha256::hex(&digest))
}

/// Constant-time check for "does this password produce the stored hash?"
/// Re-runs SHA-256 with the salt embedded in the stored string and
/// compares with [`crate::sha256::constant_time_eq`].
fn verify_hash(input: &str, stored: &str) -> bool {
    if stored.is_empty() { return true; }     // no password set
    if stored == "*" || stored == "!" { return false; }

    // Format: "$sha256$<salt>$<hex>"
    if let Some(rest) = stored.strip_prefix("$sha256$") {
        if let Some(dollar) = rest.find('$') {
            let salt = &rest[..dollar];
            let want = &rest[dollar + 1..];

            let mut sha = crate::sha256::Sha256::new();
            sha.update(salt.as_bytes());
            sha.update(b"|");
            sha.update(input.as_bytes());
            let digest = sha.finalize();
            let got = crate::sha256::hex(&digest);
            return crate::sha256::constant_time_eq(got.as_bytes(), want.as_bytes());
        }
    }
    false
}

/// Global user database
pub static USER_DB: Mutex<UserDb> = Mutex::new(UserDb::new());

/// Initialize the user database with defaults
pub fn init() {
    USER_DB.lock().init_defaults();
}

/// Current process credentials (simplified — single-process kernel)
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,  // effective UID
    pub egid: u32,  // effective GID
    pub username: String,
}

impl Default for Credentials {
    fn default() -> Self {
        Credentials {
            uid: 0,
            gid: 0,
            euid: 0,
            egid: 0,
            username: String::from("root"),
        }
    }
}

/// Global credentials for the current shell session
pub static CURRENT_CREDS: Mutex<Credentials> = Mutex::new(Credentials {
    uid: 0,
    gid: 0,
    euid: 0,
    egid: 0,
    username: String::new(),
});

/// Initialize credentials
pub fn init_creds() {
    let mut creds = CURRENT_CREDS.lock();
    creds.username = String::from("root");
}
