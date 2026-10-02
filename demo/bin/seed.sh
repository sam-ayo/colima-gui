#!/bin/sh
# Puts sample files in the uploads volume, so the Files tab has content.

set -e
cd /srv/uploads
mkdir -p products invoices avatars
for i in 1 2 3 4 5 6 7 8; do
    head -c $((RANDOM * 8 + 40000)) /dev/urandom > "products/item-$i.jpg"
done
for m in 07 08 09; do
    head -c $((RANDOM * 2 + 20000)) /dev/urandom > "invoices/2026-$m.pdf"
done
head -c 18000 /dev/urandom > avatars/ada.png
head -c 21000 /dev/urandom > avatars/grace.png
printf 'Uploads of the storefront demo.\nThe seed container made these files.\n' > README.txt
echo "seeded $(find . -type f | wc -l) files"
