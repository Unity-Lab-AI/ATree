import React from 'react';

interface User {
  id: number;
  name: string;
}

interface UserServiceProps {
  repository: UserRepository;
  logger: Logger;
}

class UserRepository {
  private db: any;

  constructor(db: any) {
    this.db = db;
  }

  findById(id: number): User | null {
    return this.db.query('SELECT * FROM users WHERE id = ?', [id]);
  }

  save(user: User): void {
    this.db.query('INSERT INTO users VALUES (?, ?)', [user.id, user.name]);
  }
}

class Logger {
  log(message: string): void {
    console.log(message);
  }
}

function UserService({ repository, logger }: UserServiceProps) {
  const findById = (id: number) => {
    logger.log(`Finding user ${id}`);
    return repository.findById(id);
  };

  return { findById };
}

export { UserRepository, Logger, UserService };
