package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.MongoCredential;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.List;

/**
 * Cross-driver dropAllUsersFromDatabase smoke — Java.
 * <p>
 * Provisions two users in {@code shop} and one in {@code other}, then
 * runs {@code dropAllUsersFromDatabase} on {@code shop}. Asserts
 * {@code n: 2} (only shop users removed), {@code shop} now has zero
 * users, and {@code other} still has Carol.
 */
public final class DropAllUsersSmoke {

    private DropAllUsersSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        String adminPwd = System.getenv("ADMIN_PASSWORD");
        if (uri == null || adminPwd == null) {
            System.err.println("MONGODB_URI and ADMIN_PASSWORD required");
            System.exit(2);
        }

        try (MongoClient root = clientFor(uri, "root", adminPwd, "admin")) {
            root.getDatabase("shop").runCommand(new Document()
                .append("createUser", "alice").append("pwd", "p")
                .append("roles", List.of(new Document()
                    .append("role", "read").append("db", "shop"))));
            root.getDatabase("shop").runCommand(new Document()
                .append("createUser", "bob").append("pwd", "p")
                .append("roles", List.of(new Document()
                    .append("role", "readWrite").append("db", "shop"))));
            root.getDatabase("other").runCommand(new Document()
                .append("createUser", "carol").append("pwd", "p")
                .append("roles", List.of(new Document()
                    .append("role", "read").append("db", "other"))));

            Document res = root.getDatabase("shop").runCommand(new Document(
                "dropAllUsersFromDatabase", 1));
            int n = res.getInteger("n", -1);
            if (n != 2) {
                System.err.printf("n: got %d, want 2%n", n);
                System.exit(1);
            }

            Document shopUsersRes = root.getDatabase("shop").runCommand(
                new Document("usersInfo", 1));
            List<?> shopUsers = (List<?>) shopUsersRes.get("users");
            if (shopUsers == null || !shopUsers.isEmpty()) {
                System.err.printf("shop users: %s%n", shopUsers);
                System.exit(1);
            }

            Document otherUsersRes = root.getDatabase("other").runCommand(
                new Document("usersInfo", 1));
            @SuppressWarnings("unchecked")
            List<Document> otherUsers = (List<Document>) otherUsersRes.get("users");
            if (otherUsers == null || otherUsers.size() != 1
                    || !"carol".equals(otherUsers.get(0).getString("user"))) {
                System.err.printf("other users: %s%n", otherUsers);
                System.exit(1);
            }

            System.out.println("OK");
        }
    }

    private static MongoClient clientFor(String uri, String user, String pwd, String authSource) {
        MongoCredential credential = MongoCredential.createScramSha256Credential(
            user, authSource, pwd.toCharArray());
        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .credential(credential)
            .applyToClusterSettings(b -> b.serverSelectionTimeout(5,
                java.util.concurrent.TimeUnit.SECONDS))
            .build();
        return MongoClients.create(settings);
    }
}
