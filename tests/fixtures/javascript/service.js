const express = require('express');

class UserService {
  constructor(repository, logger) {
    this.repository = repository;
    this.logger = logger;
  }

  findById(id) {
    this.logger.log(`Finding user ${id}`);
    return this.repository.findById(id);
  }

  save(user) {
    return this.repository.save(user);
  }
}

class UserRepository {
  constructor(db) {
    this.db = db;
  }

  findById(id) {
    return this.db.query('SELECT * FROM users WHERE id = ?', [id]);
  }

  save(user) {
    return this.db.query('INSERT INTO users VALUES (?, ?)', [user.id, user.name]);
  }
}

class Logger {
  log(message) {
    console.log(message);
  }
}

function createApp() {
  const logger = new Logger();
  const repo = new UserRepository({});
  const service = new UserService(repo, logger);
  return { service, logger, repo };
}

module.exports = { UserService, UserRepository, Logger, createApp };
