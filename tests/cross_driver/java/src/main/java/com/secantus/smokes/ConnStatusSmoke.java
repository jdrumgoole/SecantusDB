package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.MongoCredential;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.List;

/**
 * Cross-driver connectionStatus smoke — Java.
 * <p>
 * Authenticates as the bootstrap root user and asserts connectionStatus
 * surfaces the expected {@code authenticatedUsers} +
 * {@code authenticatedUserRoles} arrays.
 */
public final class ConnStatusSmoke {

    private ConnStatusSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        String adminPwd = System.getenv("ADMIN_PASSWORD");
        if (uri == null || adminPwd == null) {
            System.err.println("MONGODB_URI and ADMIN_PASSWORD required");
            System.exit(2);
        }

        MongoCredential credential = MongoCredential.createScramSha256Credential(
            "root", "admin", adminPwd.toCharArray());
        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .credential(credential)
            .applyToClusterSettings(b -> b.serverSelectionTimeout(5,
                java.util.concurrent.TimeUnit.SECONDS))
            .build();

        try (MongoClient client = MongoClients.create(settings)) {
            Document status = client.getDatabase("admin")
                .runCommand(new Document("connectionStatus", 1));
            Document authInfo = status.get("authInfo", Document.class);
            if (authInfo == null) {
                System.err.printf("connectionStatus missing authInfo: %s%n", status.toJson());
                System.exit(1);
            }
            @SuppressWarnings("unchecked")
            List<Document> users = (List<Document>) authInfo.get("authenticatedUsers");
            if (users == null || users.isEmpty()) {
                System.err.printf("authenticatedUsers empty: %s%n", authInfo.toJson());
                System.exit(1);
            }
            @SuppressWarnings("unchecked")
            List<Document> roles = (List<Document>) authInfo.get("authenticatedUserRoles");
            if (roles == null || roles.isEmpty()) {
                System.err.printf("authenticatedUserRoles empty: %s%n", authInfo.toJson());
                System.exit(1);
            }
            String firstRole = roles.get(0).getString("role");
            if (!"root".equals(firstRole)) {
                System.err.printf("expected role=root, got %s%n", roles.get(0).toJson());
                System.exit(1);
            }
            System.out.println("OK");
        }
    }
}
