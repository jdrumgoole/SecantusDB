"""Idempotent AWS provisioner + deployer for the SecantusDB marketing site.

Provisions:

* Private S3 bucket ``<domain>`` with public-access fully blocked.
* ACM certificate in ``us-east-1`` for ``<domain>`` and ``www.<domain>``,
  validated via DNS-01 records written into the existing Route 53 hosted
  zone.
* CloudFront Origin Access Control (OAC) and a CloudFront distribution
  fronting the S3 bucket. HTTPS-only, gzip+brotli compression, custom
  404, alternate domain names = apex + www.
* S3 bucket policy granting only the OAC principal ``s3:GetObject``.
* Route 53 A and AAAA alias records for apex and ``www`` pointing at
  the distribution.

State (bucket name, distribution ID, hosted-zone ID, certificate ARN)
is persisted to a JSON file so subsequent runs short-circuit and
``deploy`` can read it without re-discovering everything.

Deploy commands:

* ``sync``: ``aws s3 sync``-equivalent with cache-control headers
  (long for static assets, short for HTML/feeds).
* ``invalidate``: CloudFront ``/*`` invalidation.

Run with ``--help`` for full subcommand listing. The Route 53 hosted
zone for the domain MUST already exist; this script will not create
one (zones cost money and are typically created during domain
registration).
"""

from __future__ import annotations

import argparse
import json
import mimetypes
import sys
import time
from dataclasses import dataclass
from pathlib import Path

try:
    import boto3
    from botocore.exceptions import ClientError
except ImportError:
    sys.exit(
        "boto3 is required. Install dev/website extras: "
        "`uv sync --extra website`"
    )


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

CLOUDFRONT_HOSTED_ZONE_ID = "Z2FDTNDATAQYW2"

LONG_CACHE_EXTS = {".css", ".js", ".svg", ".png", ".jpg", ".jpeg", ".webp", ".woff", ".woff2", ".ico"}
LONG_CACHE_HEADER = "public, max-age=31536000, immutable"
SHORT_CACHE_HEADER = "public, max-age=300, must-revalidate"


# ---------------------------------------------------------------------------
# State
# ---------------------------------------------------------------------------


@dataclass
class State:
    path: Path
    data: dict[str, str]

    @classmethod
    def load(cls, path: Path) -> "State":
        if path.exists():
            return cls(path=path, data=json.loads(path.read_text()))
        return cls(path=path, data={})

    def save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(json.dumps(self.data, indent=2, sort_keys=True) + "\n")

    def set(self, key: str, value: str) -> None:
        self.data[key] = value
        self.save()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _hosted_zone_id(domain: str) -> str:
    r53 = boto3.client("route53")
    paginator = r53.get_paginator("list_hosted_zones")
    target = domain.rstrip(".") + "."
    for page in paginator.paginate():
        for zone in page["HostedZones"]:
            if zone["Name"] == target and not zone["Config"].get("PrivateZone"):
                return zone["Id"].split("/")[-1]
    raise SystemExit(
        f"No public Route 53 hosted zone found for {domain!r}. "
        "Create the zone first (or transfer the domain into Route 53)."
    )


def _ensure_bucket(bucket: str, region: str = "us-east-1") -> None:
    s3 = boto3.client("s3", region_name=region)
    try:
        s3.head_bucket(Bucket=bucket)
        print(f"  s3 bucket {bucket}: exists")
        return
    except ClientError as exc:
        code = exc.response.get("Error", {}).get("Code")
        if code not in ("404", "NoSuchBucket", "NotFound"):
            raise
    if region == "us-east-1":
        s3.create_bucket(Bucket=bucket)
    else:
        s3.create_bucket(
            Bucket=bucket,
            CreateBucketConfiguration={"LocationConstraint": region},
        )
    s3.put_public_access_block(
        Bucket=bucket,
        PublicAccessBlockConfiguration={
            "BlockPublicAcls": True,
            "IgnorePublicAcls": True,
            "BlockPublicPolicy": True,
            "RestrictPublicBuckets": True,
        },
    )
    print(f"  s3 bucket {bucket}: created (private)")


def _ensure_certificate(domain: str, hosted_zone_id: str) -> str:
    acm = boto3.client("acm", region_name="us-east-1")
    san = f"www.{domain}"
    paginator = acm.get_paginator("list_certificates")
    for page in paginator.paginate(CertificateStatuses=["PENDING_VALIDATION", "ISSUED"]):
        for cert in page["CertificateSummaryList"]:
            detail = acm.describe_certificate(CertificateArn=cert["CertificateArn"])["Certificate"]
            cert_domain = detail["DomainName"]
            sans = set(detail.get("SubjectAlternativeNames", []))
            if cert_domain == domain and san in sans:
                if detail["Status"] == "ISSUED":
                    print(f"  acm certificate for {domain}: ISSUED")
                    return cert["CertificateArn"]
                print(f"  acm certificate for {domain}: {detail['Status']} — waiting")
                _wait_cert_issued_with_dns(acm, cert["CertificateArn"], hosted_zone_id)
                return cert["CertificateArn"]

    print(f"  acm certificate for {domain}: requesting new (DNS-01)")
    arn = acm.request_certificate(
        DomainName=domain,
        SubjectAlternativeNames=[san],
        ValidationMethod="DNS",
    )["CertificateArn"]
    _wait_cert_issued_with_dns(acm, arn, hosted_zone_id)
    return arn


def _wait_cert_issued_with_dns(acm, arn: str, hosted_zone_id: str) -> None:
    r53 = boto3.client("route53")
    deadline = time.time() + 600
    written: set[str] = set()
    while time.time() < deadline:
        detail = acm.describe_certificate(CertificateArn=arn)["Certificate"]
        status = detail["Status"]
        if status == "ISSUED":
            print(f"  acm certificate: ISSUED")
            return
        if status not in ("PENDING_VALIDATION",):
            raise SystemExit(f"  acm certificate entered unexpected state: {status}")
        for option in detail.get("DomainValidationOptions", []):
            record = option.get("ResourceRecord")
            if not record or record["Name"] in written:
                continue
            r53.change_resource_record_sets(
                HostedZoneId=hosted_zone_id,
                ChangeBatch={
                    "Comment": "ACM DNS-01 validation for SecantusDB website",
                    "Changes": [{
                        "Action": "UPSERT",
                        "ResourceRecordSet": {
                            "Name": record["Name"],
                            "Type": record["Type"],
                            "TTL": 60,
                            "ResourceRecords": [{"Value": record["Value"]}],
                        },
                    }],
                },
            )
            written.add(record["Name"])
            print(f"    wrote DNS validation record {record['Name']}")
        time.sleep(15)
    raise SystemExit("  acm certificate did not validate within 10 minutes")


def _ensure_oac(name: str) -> str:
    cf = boto3.client("cloudfront")
    paginator = cf.get_paginator("list_origin_access_controls")
    for page in paginator.paginate():
        for item in page.get("OriginAccessControlList", {}).get("Items", []):
            if item["Name"] == name:
                print(f"  cloudfront OAC {name}: exists")
                return item["Id"]
    resp = cf.create_origin_access_control(
        OriginAccessControlConfig={
            "Name": name,
            "Description": "SecantusDB website OAC",
            "SigningProtocol": "sigv4",
            "SigningBehavior": "always",
            "OriginAccessControlOriginType": "s3",
        },
    )
    oac_id = resp["OriginAccessControl"]["Id"]
    print(f"  cloudfront OAC {name}: created ({oac_id})")
    return oac_id


def _ensure_distribution(domain: str, bucket: str, cert_arn: str, oac_id: str) -> tuple[str, str]:
    cf = boto3.client("cloudfront")
    paginator = cf.get_paginator("list_distributions")
    for page in paginator.paginate():
        for d in page.get("DistributionList", {}).get("Items", []) or []:
            aliases = d.get("Aliases", {}).get("Items", []) or []
            if domain in aliases:
                print(f"  cloudfront distribution: exists ({d['Id']})")
                return d["Id"], d["DomainName"]

    config = {
        "CallerReference": f"secantusdb-{int(time.time())}",
        "Aliases": {"Quantity": 2, "Items": [domain, f"www.{domain}"]},
        "DefaultRootObject": "index.html",
        "Origins": {
            "Quantity": 1,
            "Items": [{
                "Id": "s3-origin",
                "DomainName": f"{bucket}.s3.amazonaws.com",
                "S3OriginConfig": {"OriginAccessIdentity": ""},
                "OriginAccessControlId": oac_id,
                "CustomHeaders": {"Quantity": 0},
                "ConnectionAttempts": 3,
                "ConnectionTimeout": 10,
                "OriginShield": {"Enabled": False},
            }],
        },
        "DefaultCacheBehavior": {
            "TargetOriginId": "s3-origin",
            "ViewerProtocolPolicy": "redirect-to-https",
            "Compress": True,
            "AllowedMethods": {
                "Quantity": 2,
                "Items": ["GET", "HEAD"],
                "CachedMethods": {"Quantity": 2, "Items": ["GET", "HEAD"]},
            },
            "CachePolicyId": "658327ea-f89d-4fab-a63d-7e88639e58f6",
        },
        "CustomErrorResponses": {
            "Quantity": 2,
            "Items": [
                {"ErrorCode": 403, "ResponseCode": "404", "ResponsePagePath": "/404.html", "ErrorCachingMinTTL": 60},
                {"ErrorCode": 404, "ResponseCode": "404", "ResponsePagePath": "/404.html", "ErrorCachingMinTTL": 60},
            ],
        },
        "Comment": "SecantusDB marketing site",
        "Enabled": True,
        "ViewerCertificate": {
            "ACMCertificateArn": cert_arn,
            "SSLSupportMethod": "sni-only",
            "MinimumProtocolVersion": "TLSv1.2_2021",
        },
        "PriceClass": "PriceClass_100",
        "HttpVersion": "http2and3",
        "IsIPV6Enabled": True,
    }
    resp = cf.create_distribution(DistributionConfig=config)
    dist = resp["Distribution"]
    print(f"  cloudfront distribution: created ({dist['Id']}) — initial deploy may take 5-10 min")
    return dist["Id"], dist["DomainName"]


def _ensure_bucket_policy(bucket: str, distribution_id: str) -> None:
    s3 = boto3.client("s3")
    sts = boto3.client("sts")
    account = sts.get_caller_identity()["Account"]
    statement = {
        "Version": "2008-10-17",
        "Statement": [{
            "Sid": "AllowCloudFrontServicePrincipal",
            "Effect": "Allow",
            "Principal": {"Service": "cloudfront.amazonaws.com"},
            "Action": "s3:GetObject",
            "Resource": f"arn:aws:s3:::{bucket}/*",
            "Condition": {
                "StringEquals": {
                    "AWS:SourceArn": f"arn:aws:cloudfront::{account}:distribution/{distribution_id}",
                }
            },
        }],
    }
    s3.put_bucket_policy(Bucket=bucket, Policy=json.dumps(statement))
    print(f"  s3 bucket policy: granted CloudFront OAC GetObject")


def _ensure_dns_aliases(domain: str, hosted_zone_id: str, distribution_domain: str) -> None:
    r53 = boto3.client("route53")
    changes = []
    for name in (domain, f"www.{domain}"):
        for typ in ("A", "AAAA"):
            changes.append({
                "Action": "UPSERT",
                "ResourceRecordSet": {
                    "Name": name,
                    "Type": typ,
                    "AliasTarget": {
                        "HostedZoneId": CLOUDFRONT_HOSTED_ZONE_ID,
                        "DNSName": distribution_domain,
                        "EvaluateTargetHealth": False,
                    },
                },
            })
    r53.change_resource_record_sets(
        HostedZoneId=hosted_zone_id,
        ChangeBatch={"Comment": "SecantusDB site CloudFront aliases", "Changes": changes},
    )
    print(f"  route53: upserted A+AAAA aliases for {domain} and www.{domain}")


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------


def cmd_up(args: argparse.Namespace) -> None:
    state = State.load(Path(args.state_file))
    domain = args.domain
    bucket = domain
    print(f"=== Provisioning AWS infrastructure for {domain} ===")

    print("[1/7] Locating Route 53 hosted zone")
    zone_id = _hosted_zone_id(domain)
    state.set("hosted_zone_id", zone_id)
    state.set("domain", domain)
    print(f"  hosted zone: {zone_id}")

    print("[2/7] Ensuring S3 bucket")
    _ensure_bucket(bucket)
    state.set("bucket", bucket)

    print("[3/7] Ensuring ACM certificate (us-east-1)")
    cert_arn = _ensure_certificate(domain, zone_id)
    state.set("cert_arn", cert_arn)

    print("[4/7] Ensuring CloudFront OAC")
    oac_id = _ensure_oac(name=f"secantusdb-{domain.replace('.', '-')}-oac")
    state.set("oac_id", oac_id)

    print("[5/7] Ensuring CloudFront distribution")
    dist_id, dist_domain = _ensure_distribution(domain, bucket, cert_arn, oac_id)
    state.set("distribution_id", dist_id)
    state.set("distribution_domain", dist_domain)

    print("[6/7] Ensuring S3 bucket policy")
    _ensure_bucket_policy(bucket, dist_id)

    print("[7/7] Ensuring Route 53 alias records")
    _ensure_dns_aliases(domain, zone_id, dist_domain)

    print()
    print(f"Provisioning complete. State written to {state.path}")
    print(f"  bucket           = {bucket}")
    print(f"  distribution     = {dist_id}")
    print(f"  distribution dns = {dist_domain}")
    print(f"  hosted zone      = {zone_id}")
    print(f"  cert arn         = {cert_arn}")
    print()
    print("Next: `invoke deploy` to publish the first build.")


def cmd_down(args: argparse.Namespace) -> None:
    print(
        "Tear-down is destructive and requires manual confirmation. Per "
        "user-instruction policy, this command is a no-op stub. Disable "
        "the distribution / delete the bucket via the AWS console if you "
        "really want to remove the site."
    )
    sys.exit(2)


def _content_type(path: Path) -> str:
    ctype, _ = mimetypes.guess_type(path.name)
    if ctype is None:
        if path.suffix == ".svg":
            return "image/svg+xml"
        return "application/octet-stream"
    if ctype.startswith("text/") and "charset" not in ctype:
        return f"{ctype}; charset=utf-8"
    return ctype


def cmd_sync(args: argparse.Namespace) -> None:
    """Upload the build directory to S3 with appropriate cache-control headers,
    and delete remote keys that no longer exist locally."""
    bucket = args.bucket
    source = Path(args.source).resolve()
    if not source.is_dir():
        raise SystemExit(f"source directory {source} does not exist")

    s3 = boto3.client("s3")

    uploaded = 0
    local_keys: set[str] = set()
    for path in source.rglob("*"):
        if not path.is_file():
            continue
        key = path.relative_to(source).as_posix()
        local_keys.add(key)
        ctype = _content_type(path)
        cache = LONG_CACHE_HEADER if path.suffix.lower() in LONG_CACHE_EXTS else SHORT_CACHE_HEADER
        with path.open("rb") as fh:
            s3.put_object(
                Bucket=bucket,
                Key=key,
                Body=fh.read(),
                ContentType=ctype,
                CacheControl=cache,
            )
        uploaded += 1
    print(f"  uploaded {uploaded} object(s)")

    paginator = s3.get_paginator("list_objects_v2")
    to_delete: list[dict[str, str]] = []
    for page in paginator.paginate(Bucket=bucket):
        for obj in page.get("Contents", []) or []:
            if obj["Key"] not in local_keys:
                to_delete.append({"Key": obj["Key"]})
    while to_delete:
        batch, to_delete = to_delete[:1000], to_delete[1000:]
        s3.delete_objects(Bucket=bucket, Delete={"Objects": batch, "Quiet": True})
        print(f"  deleted {len(batch)} stale object(s)")


def cmd_invalidate(args: argparse.Namespace) -> None:
    cf = boto3.client("cloudfront")
    resp = cf.create_invalidation(
        DistributionId=args.distribution_id,
        InvalidationBatch={
            "Paths": {"Quantity": 1, "Items": ["/*"]},
            "CallerReference": f"deploy-{int(time.time())}",
        },
    )
    inv_id = resp["Invalidation"]["Id"]
    print(f"  invalidation {inv_id} created (status: {resp['Invalidation']['Status']})")


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="website.aws", description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_up = sub.add_parser("up", help="Provision the infrastructure (idempotent)")
    p_up.add_argument("--domain", default="secantusdb.com")
    p_up.add_argument("--state-file", required=True)
    p_up.set_defaults(func=cmd_up)

    p_down = sub.add_parser("down", help="Tear-down stub (manual via console)")
    p_down.add_argument("--domain", default="secantusdb.com")
    p_down.add_argument("--state-file", required=True)
    p_down.set_defaults(func=cmd_down)

    p_sync = sub.add_parser("sync", help="Upload site to S3")
    p_sync.add_argument("--bucket", required=True)
    p_sync.add_argument("--source", required=True)
    p_sync.set_defaults(func=cmd_sync)

    p_inv = sub.add_parser("invalidate", help="Invalidate CloudFront /*")
    p_inv.add_argument("--distribution-id", required=True)
    p_inv.set_defaults(func=cmd_invalidate)

    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()
