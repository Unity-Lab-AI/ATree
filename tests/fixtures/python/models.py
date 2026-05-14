from typing import Optional, List

class User:
    def __init__(self, id: str, name: str, email: str):
        self.id = id
        self.name = name
        self.email = email

class UserService:
    def __init__(self, repository: 'UserRepository'):
        self.repository = repository
        self.logger = Logger()

    def find_by_id(self, id: str) -> Optional[User]:
        return self.repository.find(id)

    def save(self, user: User) -> None:
        self.repository.save(user)
        self.logger.log(f"Saved user {user.id}")

class UserRepository:
    def __init__(self):
        self.users: List[User] = []

    def find(self, id: str) -> Optional[User]:
        for u in self.users:
            if u.id == id:
                return u
        return None

    def save(self, user: User) -> None:
        self.users.append(user)

class Logger:
    def log(self, message: str) -> None:
        print(message)

# Type-annotated variables
admin_user: User = User("1", "Admin", "admin@example.com")
user_ids: List[str] = ["1", "2", "3"]
