#include <stdio.h>
#include <stdlib.h>

// User struct
typedef struct {
    int id;
    char name[64];
    char email[128];
} User;

// Logger struct
typedef struct {
    char prefix[32];
} Logger;

// Repository interface (function pointers)
typedef struct {
    User* (*find_by_id)(int id);
    void (*save)(User* user);
    int (*count)(void);
} Repository;

// UserService struct
typedef struct {
    Repository* repo;
    Logger* logger;
} UserService;

// Function declarations
void logger_log(Logger* logger, const char* message);
User* user_service_find_by_id(UserService* service, int id);
void user_service_save(UserService* service, User* user);
int user_service_count(UserService* service);
UserService* user_service_new(Repository* repo, Logger* logger);

// Logger implementation
void logger_log(Logger* logger, const char* message) {
    printf("[%s] %s\n", logger->prefix, message);
}

// UserService implementation
User* user_service_find_by_id(UserService* service, int id) {
    return service->repo->find_by_id(id);
}

void user_service_save(UserService* service, User* user) {
    service->repo->save(user);
}

int user_service_count(UserService* service) {
    return service->repo->count();
}

UserService* user_service_new(Repository* repo, Logger* logger) {
    UserService* s = (UserService*)malloc(sizeof(UserService));
    s->repo = repo;
    s->logger = logger;
    return s;
}
