package main

import (
	"fmt"
	"log"
)

// User represents a user in the system
type User struct {
	ID    int
	Name  string
	Email string
}

// Repository defines the interface for data access
type Repository interface {
	FindByID(id int) (*User, error)
	Save(user *User) error
	Count() int
}

// UserService provides user operations
type UserService struct {
	repo   Repository
	logger *log.Logger
}

// NewUserService creates a new UserService
func NewUserService(repo Repository, logger *log.Logger) *UserService {
	return &UserService{repo: repo, logger: logger}
}

// FindByID finds a user by ID
func (s *UserService) FindByID(id int) (*User, error) {
	s.logger.Printf("Finding user %d", id)
	return s.repo.FindByID(id)
}

// Save saves a user
func (s *UserService) Save(user *User) error {
	return s.repo.Save(user)
}

// Count returns the number of users
func (s *UserService) Count() int {
	return s.repo.Count()
}

// Logger provides logging
type Logger struct {
	prefix string
}

// Log logs a message
func (l *Logger) Log(message string) {
	fmt.Printf("[%s] %s\n", l.prefix, message)
}
