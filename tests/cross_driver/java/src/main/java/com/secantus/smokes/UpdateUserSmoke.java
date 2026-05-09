package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.MongoCredential;
import com.mongodb.MongoSecurityException;
import com.mongodb.MongoTimeoutException;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.List;

/**
 * Cross-driver updateUser smoke — Java.
 * <p>
 * Provisions {@code alice_xd} with password {@code orig}, rotates to
 * {@code rotated} via updateUser, then asserts the old password no
 * longer authenticates and the new one does.
 */
public final class UpdateUserSmoke {

    private UpdateUserSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        String adminPwd = System.getenv("ADMIN_PASSWORD");
        if (uri == null || adminPwd == null) {
            System.err.println("MONGODB_URI and ADMIN_PASSWORD required");
            System.exit(2);
        }

        try (MongoClient root = clientFor(uri, "root", adminPwd, "admin")) {
            root.getDatabase("admin").runCommand(new Document()
                .append("createUser", "alice_xd")
                .append("pwd", "orig")
                .append("roles", List.of(new Document()
                    .append("role", "read")
                    .append("db", "admin"))));
            root.getDatabase("admin").runCommand(new Document()
                .append("updateUser", "alice_xd")
                .append("pwd", "rotated"));
        }

        // Old password — must fail.
        boolean oldFailed = false;
        try (MongoClient old = clientFor(uri, "alice_xd", "orig", "admin")) {
            old.getDatabase("admin").runCommand(new Document("ping", 1));
        } catch (MongoSecurityException | MongoTimeoutException ex) {
            oldFailed = true;
        }
        if (!oldFailed) {
            System.err.println("old password should not authenticate");
            System.exit(1);
        }

        // New password — must succeed.
        try (MongoClient fresh = clientFor(uri, "alice_xd", "rotated", "admin")) {
            fresh.getDatabase("admin").runCommand(new Document("ping", 1));
        }

        System.out.println("OK");
    }

    private static MongoClient clientFor(String uri, String user, String pwd, String authSource) {
        MongoCredential credential = MongoCredential.createScramSha256Credential(
            user, authSource, pwd.toCharArray());
        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .credential(credential)
            .applyToClusterSettings(b -> b.serverSelectionTimeout(30,
                java.util.concurrent.TimeUnit.SECONDS))
            .build();
        return MongoClients.create(settings);
    }
}
