use std::fmt;

pub struct User {
    pub id: String,
    pub name: String,
    pub email: String,
}

impl fmt::Display for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "User({})", self.id)
    }
}

pub struct UserService {
    users: Vec<User>,
    logger: Logger,
}

impl UserService {
    pub fn new(logger: Logger) -> Self {
        Self {
            users: Vec::new(),
            logger,
        }
    }

    pub fn find_by_id(&self, id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    pub fn save(&mut self, user: User) {
        self.logger.log(&format!("Saved user {}", user.id));
        self.users.push(user);
    }

    pub fn count(&self) -> usize {
        self.users.len()
    }
}

pub struct Logger;

impl Logger {
    pub fn log(&self, message: &str) {
        println!("{}", message);
    }
}

pub trait Repository {
    fn find(&self, id: &str) -> Option<&User>;
    fn save(&mut self, user: User);
}

impl Repository for UserService {
    fn find(&self, id: &str) -> Option<&User> {
        self.find_by_id(id)
    }

    fn save(&mut self, user: User) {
        self.save(user);
    }
}
