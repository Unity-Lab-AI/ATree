import Foundation

// User struct
struct User {
    let id: Int
    let name: String
    let email: String
}

// Repository protocol
protocol Repository {
    func findById(_ id: Int) -> User?
    func save(_ user: User)
    func count() -> Int
}

// Logger protocol
protocol Logger {
    func log(_ message: String)
}

// UserService class
class UserService {
    private let repo: Repository
    private let logger: Logger

    init(repo: Repository, logger: Logger) {
        self.repo = repo
        self.logger = logger
    }

    func findById(_ id: Int) -> User? {
        logger.log("Finding user \(id)")
        return repo.findById(id)
    }

    func save(_ user: User) {
        repo.save(user)
    }

    func count() -> Int {
        return repo.count()
    }
}

// AbstractController class
class AbstractController {
    let viewPath: String

    init(viewPath: String) {
        self.viewPath = viewPath
    }

    func render(_ template: String) -> String {
        return "\(viewPath)/\(template)"
    }
}

// UserController extends AbstractController
class UserController: AbstractController {
    private let userService: UserService

    init(userService: UserService) {
        self.userService = userService
        super.init(viewPath: "users")
    }

    func index() -> String { return render("index") }
    func show(_ id: Int) -> String {
        let user = userService.findById(id)
        return render("show")
    }
}

// Utils enum (namespace)
enum Utils {
    static func formatName(_ name: String) -> String {
        return "User: \(name)"
    }
}
