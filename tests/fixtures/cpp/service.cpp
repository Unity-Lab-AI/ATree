#include <string>
#include <vector>
#include <iostream>

// User class
class User {
public:
    int id;
    std::string name;
    std::string email;

    User(int id, const std::string& name, const std::string& email)
        : id(id), name(name), email(email) {}

    int getId() const { return id; }
    std::string getName() const { return name; }
};

// Repository abstract class
class Repository {
public:
    virtual ~Repository() = default;
    virtual User* findById(int id) = 0;
    virtual void save(User* user) = 0;
    virtual int count() = 0;
};

// Logger class
class Logger {
private:
    std::string prefix;
public:
    Logger(const std::string& p) : prefix(p) {}
    void log(const std::string& message) {
        std::cout << "[" << prefix << "] " << message << std::endl;
    }
};

// UserService class
class UserService {
private:
    Repository* repo;
    Logger* logger;
public:
    UserService(Repository* repo, Logger* logger)
        : repo(repo), logger(logger) {}

    User* findById(int id) {
        logger->log("Finding user " + std::to_string(id));
        return repo->findById(id);
    }

    void save(User* user) {
        repo->save(user);
    }

    int count() {
        return repo->count();
    }
};

// AbstractController
class AbstractController {
protected:
    std::string viewPath;
public:
    AbstractController(const std::string& path) : viewPath(path) {}
    virtual ~AbstractController() = default;
    std::string render(const std::string& templateName) {
        return viewPath + "/" + templateName;
    }
};

// UserController extends AbstractController
class UserController : public AbstractController {
private:
    UserService* userService;
public:
    UserController(UserService* service)
        : AbstractController("users"), userService(service) {}

    std::string index() { return render("index"); }
    std::string show(int id) {
        User* user = userService->findById(id);
        return render("show");
    }
};

// Utils namespace
namespace Utils {
    std::string formatName(const std::string& name) {
        return "User: " + name;
    }

    class StringHelper {
    public:
        static std::string trim(const std::string& s) { return s; }
    };
}
