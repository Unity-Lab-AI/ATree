<?php

namespace App\Controller;

use App\Service\UserService;
use App\Entity\User;

abstract class AbstractController {
    protected function render(string $template): string {
        return "rendered: " . $template;
    }
}

class UserController extends AbstractController {
    private UserService $userService;

    public function __construct(UserService $userService) {
        $this->userService = $userService;
    }

    public function index(): string {
        $users = $this->userService->findAll();
        return $this->render('users/index');
    }

    public function show(int $id): string {
        $user = $this->userService->findById($id);
        return $this->render('users/show');
    }

    public function save(array $data): string {
        $user = new User($data['name'], $data['email']);
        $this->userService->save($user);
        return $this->render('users/saved');
    }
}

class UserService {
    private array $users = [];

    public function findAll(): array {
        return $this->users;
    }

    public function findById(int $id): ?User {
        return $this->users[$id] ?? null;
    }

    public function save(User $user): void {
        $this->users[] = $user;
    }
}

class User {
    public string $name;
    public string $email;

    public function __construct(string $name, string $email) {
        $this->name = $name;
        $this->email = $email;
    }
}
