import express from 'express';
const app = express();
const router = express.Router();

// Express routes
app.get('/users', (req, res) => { res.json([]); });
app.post('/users', (req, res) => { res.status(201).json({}); });
app.get('/users/:id', (req, res) => { res.json({}); });
router.put('/users/:id', (req, res) => { res.json({}); });
app.delete('/users/:id', handler);

// Class with typed members
class UserService {
    private users: User[];
    protected logger: Logger;

    constructor(logger: Logger) {
        this.users = [];
        this.logger = logger;
    }

    async findById(id: string): Promise<User | undefined> {
        return this.users.find(u => u.id === id);
    }

    async save(user: User): Promise<void> {
        this.users.push(user);
        this.logger.log(`Saved user ${user.id}`);
    }
}

interface User {
    id: string;
    name: string;
    email: string;
}

class Logger {
    log(message: string): void {
        console.log(message);
    }
}

function handler(req: any, res: any) {
    res.send('ok');
}
