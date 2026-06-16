// User data class
data class User(val id: Int, val name: String, val email: String)

// Repository interface
interface Repository {
    fun findById(id: Int): User?
    fun save(user: User)
    fun count(): Int
}

// Logger interface
interface Logger {
    fun log(message: String)
}

// UserService class
class UserService(
    private val repo: Repository,
    private val logger: Logger
) {
    fun findById(id: Int): User? {
        logger.log("Finding user $id")
        return repo.findById(id)
    }

    fun save(user: User) = repo.save(user)
    fun count(): Int = repo.count()
}

// AbstractController open class
abstract class AbstractController(protected val viewPath: String) {
    fun render(template: String): String = "$viewPath/$template"
}

// UserController extends AbstractController
class UserController(
    private val userService: UserService
) : AbstractController("users") {
    fun index(): String = render("index")
    fun show(id: Int): String {
        val user = userService.findById(id)
        return render("show")
    }
}

// Utils object
object Utils {
    fun formatName(name: String) = "User: $name"
}
